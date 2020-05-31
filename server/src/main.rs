use futures::future::join_all;
use generational_arena::{Arena, Index};
use std::time::SystemTime;
use nalgebra::Vector2;
use std::error::Error;
use std::net::SocketAddr;
use std::num::Wrapping;
use std::time::Duration;
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

}

fn accept(players: &mut Arena<Player>, stream: TcpStream, mut internal_tcp_tx: Sender<(Index, Option<game::TcpServerMessage>)>, tick_rate: u32, tick_zero: SystemTime, tick: game::Tick) {

    println!("connection!");

    let (tx, mut rx) = channel(4);
    let idx = match players.try_insert(Player {
        player: game::Player {
            id: 0,  // Set below.
            radius: 1.0,
            position: Vector2::new(0.0, 0.0),
            velocity: Vector2::new(0.0, 0.0),
        },
        tcp_tx: tx,
        udp_addr: None,
        random_bytes: rand::random(),
        input: Vector2::new(0.0, 0.0),
    }) {
        Ok(idx) => idx,
        Err(_) => {
            println!("rejecting connection; too many players");
            return
        },
    };
    // Set the user ID to the arena index.
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
        let join_handles = tcp_txs
            .iter_mut()
            .map(|tcp_tx| tcp_tx.send(msg.clone()));
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
                    break
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
                    break
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
        })).unwrap();
        if let Err(err) = writer.write_all(&bytes[..]).await {
            eprintln!("{}", err);
            return
        }
        println!("wrote init message");

        loop {
            match rx.recv().await {
                Some(msg) => {
                    if let Err(_) = writer.write_all(bincode::serialize(&msg).unwrap().as_slice()).await {
                        break
                    }
                },
                None => break,
            };
        }
    });

}

fn step(players: &mut Arena<Player>, dt: f64) {

    // Manage collisions.

    // We have to collect the idxs to avoid borrowing `players`.
    let idxs: Vec<_> = players
        .iter()
        .map(|(idx, _)| idx)
        .collect();

    let idx_pairs = idxs
        .iter()
        .map(|a| {
            idxs.iter().map(move |b| (a, b))
        })
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
            continue  // No collision.
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

    // Apply player impulse.
    for (_, player) in players.iter_mut() {
        let acceleration = 64.0;
        let max_velocity = 16.0;
        let friction = 16.0;

        // Acceleration ranges from `friction` to `friction + acceleration`,
        // and is inversely proportional to the projection of the current velocity onto the input vector.
        let acceleration_index = player.player.velocity.dot(&player.input) / max_velocity;
        let acceleration_index = if acceleration_index < 0.0 { 0.0 } else { acceleration_index.sqrt() };
        let adjusted_acceleration = friction + acceleration * (1.0 - acceleration_index);
        player.player.velocity += adjusted_acceleration * dt * player.input;

        let dampened_velocity_unclamped = player.player.velocity.magnitude() - dt * friction;
        let dampened_velocity = if dampened_velocity_unclamped < 0.0 { 0.0 } else { dampened_velocity_unclamped };
        let velocity_unit = player.player.velocity.try_normalize(0.0).unwrap_or(Vector2::new(0.0, 0.0));
        player.player.velocity = dampened_velocity * velocity_unit;

    }

    for (_, player) in players.iter_mut() {
        player.player.position += dt * player.player.velocity;
    }

}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {

    let max_players = 16;
    let mut players: Arena<Player> = Arena::with_capacity(max_players);

    let tick_rate = 10;
    let tick_period = Duration::from_secs(1) / tick_rate;
    let mut ticker = interval(tick_period);

    let mut tcp_listener = TcpListener::bind("0.0.0.0:5000").await?;
    let mut udp_socket = UdpSocket::bind("0.0.0.0:5000").await?;

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

                println!(
                    "{} {:.2}",
                    tick,
                    SystemTime::now().duration_since(tick_zero)?.as_secs_f64() % (1.0 / tick_rate as f64),
                );

                // Update game state.
                let dt = 1.0 / tick_rate as f64; // TODO(jack) Measure actual elapsed time.
                step(&mut players, dt);

                // Broadcast.
                let update = game::WorldUpdate {
                    tick: tick.0,
                    players: players.iter().map(|(_, p)| p.player).collect(),
                };
                let bytes = bincode::serialize(&game::UdpClientMessage::WorldUpdate(update)).unwrap();
                for (_, player) in players.iter().filter(|(_, p)| p.udp_addr.is_some()) {
                    udp_socket.send_to(&bytes, player.udp_addr.unwrap()).await?;
                }

                tick = tick + Wrapping(1);

            },

            accept_result = tcp_listener.accept() => match accept_result {
                Ok((stream, _)) => accept(&mut players, stream, tcp_tx.clone(), tick_rate, tick_zero, tick.0),
                Err(err) => {
                    eprintln!("{}", err);
                    break
                },
            },

            // TODO(jack) TCP messages from the client should end up in a channel.
            recv_result = tcp_rx.recv() => match recv_result {
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

            recv_from_result = udp_socket.recv_from(&mut buf) => match recv_from_result {
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
                        game::UdpServerMessage::PlayerInput(game::PlayerInput {up, left, down, right}) => {
                            let (_, player) = match players.iter_mut().find(|(_, player)| player.udp_addr.is_some() && player.udp_addr.unwrap() == socket_addr) {
                                Some((idx, player)) => (idx, player),
                                None => continue,
                            };
                            player.input = Vector2::new(
                                (right as i32 - left as i32) as f64,
                                (down as i32 - up as i32) as f64,
                            ).try_normalize(0.0).unwrap_or(Vector2::new(0.0, 0.0));
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
