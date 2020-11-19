use futures::future::join_all;
use generational_arena::{Arena, Index};
use nalgebra::Vector2;
use std::env::args;
use std::error::Error;
use std::net::SocketAddr;
use std::num::Wrapping;
use std::time::Duration;
use std::time::SystemTime;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::prelude::*;
use tokio::sync::mpsc::{channel, Sender};
use tokio::time::interval;

use game;

#[derive(Debug)]
struct Player {
    player: game::Player,

    tcp_tx: Sender<game::TcpClientMessage>,

    random_bytes: [u8; game::NUM_RANDOM_BYTES],
    udp_addr: Option<SocketAddr>,

    input: Vector2<f64>,
    angle: f64,
    firing: bool,
    fire_counter: f64,
}

#[derive(Debug)]
struct Bullet {
    bullet: game::Bullet,
    velocity: Vector2<f64>,
    lifetime: f64,
}

fn accept(
    players: &mut Arena<Player>,
    bullets: &Arena<Bullet>,
    stream: TcpStream,
    mut internal_tcp_tx: Sender<(Index, Option<game::TcpServerMessage>)>,
    tick_rate: u32,
    tick_zero: SystemTime,
    tick: game::Tick,
) {
    println!("connection!");

    let (tx, mut rx) = channel(4);
    let idx = match players.try_insert(Player {
        player: game::Player {
            id: 0, // Set below.
            radius: 1.0,
            position: Vector2::new(0.0, 0.0),
            velocity: Vector2::new(0.0, 0.0),
        },
        tcp_tx: tx,
        udp_addr: None,
        random_bytes: rand::random(),
        input: Vector2::new(0.0, 0.0),
        angle: 0.0,
        firing: false,
        fire_counter: 0.0,
    }) {
        Ok(idx) => idx,
        Err(_) => {
            println!("rejecting connection; too many players");
            return;
        }
    };
    // Set the user ID to some combinaiton of the arena index and generation.
    let id = idx.into_raw_parts().0 as u8;
    players[idx].player.id = id;

    // TODO(jack) Broadcast PlayerLeft messages.

    // Broadcast PlayerJoined messages.
    let mut tcp_txs: Vec<_> = players
        .iter()
        .filter(|(other_idx, _)| *other_idx != idx)
        .map(|(_, p)| p.tcp_tx.clone())
        .collect();
    let msg = game::TcpClientMessage::PlayerJoined(id);
    tokio::spawn(async move {
        let join_handles = tcp_txs.iter_mut().map(|tcp_tx| tcp_tx.send(msg.clone()));
        join_all(join_handles).await;
    });

    // Start tasks to read-from / write-to the TCP socket.

    let (mut reader, mut writer) = stream.into_split();

    tokio::spawn(async move {
        loop {
            const MAX_PACKET_SIZE_PLUS_ONE: usize = 64;
            let mut buf: [u8; MAX_PACKET_SIZE_PLUS_ONE] = [0; MAX_PACKET_SIZE_PLUS_ONE];
            let num_bytes = match reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(MAX_PACKET_SIZE_PLUS_ONE) => break,
                Err(err) => {
                    eprintln!("{}", err);
                    break;
                }
                Ok(num_bytes) => num_bytes,
            };
            match bincode::deserialize(&buf[..num_bytes]) {
                Ok(msg) => match internal_tcp_tx.send((idx, Some(msg))).await {
                    Ok(_) => (),
                    Err(_) => break,
                },
                Err(err) => {
                    eprintln!("{}", err);
                    break;
                }
            };
        }
        // One consequence of every client publishing TCP packets to the same channel
        // is that we don't know when any one disconnects.
        // We signal it here with a `None`.
        internal_tcp_tx.send((idx, None)).await.ok();
    });

    let random_bytes = players[idx].random_bytes.clone();

    let update = game::WorldUpdate {
        tick,
        players: players.iter().map(|(_, p)| p.player).collect(),
        bullets: bullets.iter().map(|(_, b)| b.bullet).collect(),
    };

    tokio::spawn(async move {
        // Send the init packet.
        // For now, this will just include a random sequence of bytes.
        // We'll then wait for the random sequence of bytes via UDP to identify the client's external port number.
        let bytes = bincode::serialize(&game::TcpClientMessage::Init(game::ClientInit {
            id,
            random_bytes,
            update,
            tick_rate: tick_rate as u8,
            tick_zero,
        }))
        .unwrap();
        if let Err(err) = writer.write_all(&bytes[..]).await {
            eprintln!("{}", err);
            return;
        }
        println!("wrote init message");

        loop {
            match rx.recv().await {
                Some(msg) => {
                    if let Err(_) = writer
                        .write_all(bincode::serialize(&msg).unwrap().as_slice())
                        .await
                    {
                        break;
                    }
                }
                None => break,
            };
        }
    });
}

fn step(players: &mut Arena<Player>, bullets: &mut Arena<Bullet>, dt: f64) {
    // Apply player impulse.
    for (_, player) in players.iter_mut() {
        let acceleration = 64.0;
        let max_velocity = 16.0;
        let friction = 16.0;

        // Acceleration ranges from `friction` to `friction + acceleration`,
        // and is inversely proportional to the projection of the current velocity onto the input vector.
        let acceleration_index = player.player.velocity.dot(&player.input) / max_velocity;
        let acceleration_index = if acceleration_index < 0.0 {
            0.0
        } else {
            acceleration_index.sqrt()
        };
        let adjusted_acceleration = friction + acceleration * (1.0 - acceleration_index);
        player.player.velocity += adjusted_acceleration * dt * player.input;

        let dampened_velocity_unclamped = player.player.velocity.magnitude() - dt * friction;
        let dampened_velocity = if dampened_velocity_unclamped < 0.0 {
            0.0
        } else {
            dampened_velocity_unclamped
        };
        let velocity_unit = player
            .player
            .velocity
            .try_normalize(0.0)
            .unwrap_or(Vector2::new(0.0, 0.0));
        player.player.velocity = dampened_velocity * velocity_unit;

        player.player.position += dt * player.player.velocity;
    }

    // Remove expired bullets.
    let bullets_to_remove: Vec<_> = bullets
        .iter()
        .filter(|(_, b)| b.lifetime > 1.0)
        .map(|(idx, _)| idx)
        .collect();
    for idx in bullets_to_remove.iter() {
        bullets.remove(*idx);
    }

    // Fire bullets.
    for (_, player) in players.iter_mut().filter(|(_, p)| p.firing) {
        let rof = 30.0;
        player.fire_counter += rof * dt;
        if player.fire_counter >= 1.0 {
            player.fire_counter %= 1.0;
            let idx = match bullets.try_insert(Bullet {
                bullet: game::Bullet {
                    id: 0,
                    player_id: player.player.id,
                    position: player.player.position,
                    angle: player.angle,
                    radius: 0.5,
                },
                velocity: 32.0 * Vector2::new(player.angle.cos(), player.angle.sin()),
                lifetime: 0.0,
            }) {
                Ok(idx) => idx,
                Err(_) => {
                    eprintln!("too many bullets!");
                    break;
                }
            };
            // Set the user ID to the arena index.
            let raw_parts = idx.into_raw_parts();
            bullets[idx].bullet.id = ((raw_parts.0 & 10) | ((raw_parts.1 as usize) << 10)) as u16;
        }
    }

    // Update bullets.
    for (_, bullet) in bullets.iter_mut() {
        bullet.bullet.position += dt * bullet.velocity;
        bullet.lifetime += dt;
    }

    // Manage collisions.

    // We have to collect the idxs to avoid borrowing `players`.
    let idxs: Vec<_> = players.iter().map(|(idx, _)| idx).collect();

    let idx_pairs = idxs
        .iter()
        .map(|a| idxs.iter().map(move |b| (a, b)))
        .flatten()
        .filter(|(a, b)| a.into_raw_parts().0 < b.into_raw_parts().0);

    for (a, b) in idx_pairs {
        let (a, b) = players.get2_mut(*a, *b);
        let a = a.unwrap();
        let b = b.unwrap();

        let distance = match a.player.position - b.player.position {
            v if v.x == 0.0 && v.y == 0.0 => Vector2::new(0.001, 0.001),
            v => v,
        };

        let max_distance = a.player.radius + b.player.radius;
        if distance.magnitude_squared() >= max_distance.powi(2) {
            continue; // No collision.
        }

        let displacement_unit = distance.try_normalize(0.0).unwrap();
        let displacement = displacement_unit * (max_distance - distance.magnitude());
        a.player.position += 0.5 * displacement;
        b.player.position += -0.5 * displacement;

        let momentum = a.player.velocity.magnitude() + b.player.velocity.magnitude();
        let elasticity = 2.0;
        a.player.velocity = 0.5 * elasticity * momentum * displacement_unit;
        b.player.velocity = -0.5 * elasticity * momentum * displacement_unit;
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let port: u32 = match args().nth(1).and_then(|s| s.parse().ok()) {
        Some(port) => port,
        None => {
            eprintln!("Usage: {} PORT", args().nth(0).unwrap());
            return Ok(());
        }
    };

    let mut players = Arena::with_capacity(16);
    let mut bullets = Arena::with_capacity(1024);

    let tick_rate = 60;
    let mut ticker = interval(Duration::from_secs(1) / tick_rate);

    let snapshot_rate = 10;
    let mut snapshot_ticker = interval(Duration::from_secs(1) / snapshot_rate);

    let mut tcp_listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
    let mut udp_socket = UdpSocket::bind(format!("0.0.0.0:{}", port)).await?;

    // tcp_rx is our global receiver for TCP events.
    // This means that each player holds a copy of tcp_tx which packets are passed to.
    let (tcp_tx, mut tcp_rx) = channel(4);

    let mut tick = Wrapping(0);
    let tick_zero = SystemTime::now();

    loop {
        const MAX_PACKET_SIZE_PLUS_ONE: usize = 64;
        let mut buf: [u8; MAX_PACKET_SIZE_PLUS_ONE] = [0; MAX_PACKET_SIZE_PLUS_ONE];

        // TODO(jack) Redesign this select! call to execute as little code linearly as possible.

        tokio::select! {

            _ = ticker.tick() => {

                // Update game state.
                let dt = 1.0 / tick_rate as f64; // TODO(jack) Measure actual elapsed time.
                step(&mut players, &mut bullets, dt);

                tick = tick + Wrapping(1);

            },

            _ = snapshot_ticker.tick() => {

                // Broadcast.
                let update = game::WorldUpdate {
                    tick: tick.0,
                    players: players.iter().map(|(_, p)| p.player).collect(),
                    bullets: bullets.iter().map(|(_, b)| b.bullet).collect(),
                };
                let bytes = bincode::serialize(&game::UdpClientMessage::WorldUpdate(update)).unwrap();
                for (_, player) in players.iter().filter(|(_, p)| p.udp_addr.is_some()) {
                    udp_socket.send_to(&bytes, player.udp_addr.unwrap()).await?;
                }

            },

            accept_result = tcp_listener.accept() => match accept_result {
                Ok((stream, _)) => accept(&mut players, &bullets, stream, tcp_tx.clone(), tick_rate, tick_zero, tick.0),
                Err(err) => {
                    eprintln!("{}", err);
                    break
                },
            },

            // TODO(jack) TCP messages from the client should end up in a channel.
            result = tcp_rx.recv() => match result {
                Some((idx, None)) => {
                    println!("disconnection!");
                    let id = players[idx].player.id;

                    // Broadcast that a player left.
                    let mut tcp_txs: Vec<_> = players
                        .iter()
                        .filter(|(other_idx, _)| *other_idx != idx)
                        .map(|(_, p)| p.tcp_tx.clone())
                        .collect();
                    tokio::spawn(async move {
                        let msg = game::TcpClientMessage::PlayerLeft(id);
                        let join_handles = tcp_txs
                            .iter_mut()
                            .map(|tcp_tx| tcp_tx.send(msg.clone())); // TODO(jack) Can we do this without allocations?
                        join_all(join_handles).await;
                    });
                    players.remove(idx);

                },
                Some((idx, Some(msg))) => println!("{:?}: {:?}", idx, msg),
                None => break,
            },

            result = udp_socket.recv_from(&mut buf) => match result {
                Ok((0, _)) => break,
                Ok((MAX_PACKET_SIZE_PLUS_ONE, _)) => break,
                Ok((num_bytes, socket_addr)) => {

                    let bytes = &buf[..num_bytes];
                    let msg: game::UdpServerMessage = match bincode::deserialize(&bytes) {
                        Ok(msg) => msg,
                        Err(err) => {
                            eprintln!("{}", err);
                            continue
                        },
                    };

                    match msg {
                        game::UdpServerMessage::Init(game::ServerInit { random_bytes }) => {
                            println!("received init message: {:?}", random_bytes);
                            if let Some((_, player)) = players.iter_mut().find(|(_, player)| player.random_bytes == random_bytes) {
                                player.udp_addr = Some(socket_addr);
                                println!("{:?}", player.udp_addr);
                            }
                        },
                        game::UdpServerMessage::PlayerInput(inputs) => {
                            let (_, player) = match players.iter_mut().find(|(_, player)| player.udp_addr.is_some() && player.udp_addr.unwrap() == socket_addr) {
                                Some((idx, player)) => (idx, player),
                                None => continue,
                            };
                            // TODO(jack) Apply the inputs according to their tick.
                            // Right now, we're just taking the most recent one.
                            if inputs.len() == 0 {
                                continue
                            }
                            let input = inputs.iter().last().unwrap();
                            player.input = Vector2::new(
                                (input.right as i32 - input.left as i32) as f64,
                                (input.down as i32 - input.up as i32) as f64,
                            ).try_normalize(0.0).unwrap_or(Vector2::new(0.0, 0.0));

                            player.angle = input.angle;

                            // TODO(jack) We probably just want to compose input in the player struct.
                            if input.mouse_left {
                                player.firing = true;
                            } else {
                                player.firing = false;
                                player.fire_counter = 0.0;
                            }

                        },
                    };

                },
                Err(err) => {
                    eprintln!("{}", err);
                    break
                },
            }

        }
    }
    Ok(())
}
