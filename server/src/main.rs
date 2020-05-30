use generational_arena::Arena;
use nalgebra::Vector2;
use std::error::Error;
use std::net::SocketAddr;
use std::str::from_utf8;
use std::time::Duration;
use tokio::net::{TcpListener, UdpSocket};
use tokio::prelude::*;
use tokio::sync::mpsc::{channel, Sender};

use game;

#[derive(Debug)]
struct Player {
    tcp_tx: Sender<Vec<u8>>,

    random_bytes: [u8; game::NUM_RANDOM_BYTES],
    udp_addr: Option<SocketAddr>,

    position: Vector2<f64>,
    velocity: Vector2<f64>,
    input: Vector2<f64>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let max_players = 16;
    let mut players: Arena<Player> = Arena::with_capacity(max_players);

    let tick_rate = 60;
    let tick_period = Duration::from_secs(1) / tick_rate;
    let mut tick_interval = tokio::time::interval(tick_period);

    let mut tcp_listener = TcpListener::bind("127.0.0.1:5000").await?;
    let mut udp_socket = UdpSocket::bind("127.0.0.1:5000").await?;

    let (tcp_tx, mut tcp_rx) = channel(4);

    loop {

        const MAX_PACKET_SIZE_PLUS_ONE: usize = 64;
        let mut buf: [u8; MAX_PACKET_SIZE_PLUS_ONE] = [0; MAX_PACKET_SIZE_PLUS_ONE];

        tokio::select! {

            _ = tick_interval.tick() => {

                // Update game state.
                let dt = 1.0 / tick_rate as f64; // TODO(jack) Measure actual elapsed time.
                for (_, player) in players.iter_mut() {

                    // Acceleration / deceleration time in seconds.
                    let acceleration = 0.05;
                    let friction = 0.7;
                    let velocity_max = 1.0;

                    let acceleration = 2.0 * velocity_max / acceleration;
                    let friction = velocity_max / friction;

                    // let adjusted_acceleration = acceleration * (1.0 - player.velocity.dot(&player.input) / velocity_max) + friction;
                    let adjusted_acceleration = acceleration * (1.0 - player.velocity.magnitude() / velocity_max) + friction;
                    let new_velocity = player.velocity + adjusted_acceleration * player.input * dt;

                    // Apply friction.
                    let new_velocity_magnitude_unclamped = new_velocity.magnitude() - dt * friction;
                    let new_velocity_magnitude = if new_velocity_magnitude_unclamped < 0.0 { 0.0 } else { new_velocity_magnitude_unclamped };
                    player.velocity = new_velocity_magnitude * new_velocity.try_normalize(0.0).unwrap_or(Vector2::new(0.0, 0.0));

                    player.position += dt * player.velocity;

                }

                // Broadcast.
                for (_, player) in players.iter().filter(|(_, p)| p.udp_addr.is_some()) {
                    let msg = game::UdpClientMessage::PlayerUpdate( game::PlayerUpdate {
                        x: player.position.x as f32,
                        y: player.position.y as f32,
                    });
                    let bytes = bincode::serialize(&msg).unwrap();
                    udp_socket.send_to(&bytes, player.udp_addr.unwrap()).await?;
                }

            },

            accept_result = tcp_listener.accept() => match accept_result {
                Ok(_) if players.len() >= max_players => println!("rejecting connection; too many players"),
                Ok((stream, _)) => {

                    println!("connection!");

                    let (tx, mut rx) = channel(4);
                    let idx = players.insert(Player {
                        tcp_tx: tx,
                        udp_addr: None,
                        random_bytes: rand::random(),
                        position: Vector2::new(0.0, 0.0),
                        velocity: Vector2::new(0.0, 0.0),
                        input: Vector2::new(0.0, 0.0),
                    });

                    // Start tasks to read-from / write-to the TCP socket.

                    let (mut reader, mut writer) = stream.into_split();

                    let mut tcp_tx = tcp_tx.clone();
                    tokio::spawn(async move {
                        const MAX_PACKET_SIZE_PLUS_ONE: usize = 64;
                        let mut buf: [u8; MAX_PACKET_SIZE_PLUS_ONE] = [0; MAX_PACKET_SIZE_PLUS_ONE];
                        loop {
                            match reader.read(&mut buf).await {
                                Ok(0) => break,
                                Ok(MAX_PACKET_SIZE_PLUS_ONE) => break,
                                Ok(num_bytes) => match tcp_tx.send((idx, buf[..num_bytes].to_owned())).await {
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
                        // For now, we'll send an empty vec down the channel.
                        tcp_tx.send((idx, Vec::new())).await.ok();
                    });

                    let random_bytes = players[idx].random_bytes.clone();
                    println!("{:?}", random_bytes);

                    tokio::spawn(async move {

                        // Send the init packet.
                        // For now, this will just include a random sequence of bytes.
                        // We'll then wait for the random sequence of bytes via UDP to identify the client's external port number.
                        let bytes = bincode::serialize(&game::TcpClientMessage::Init(game::Init {
                            random_bytes,
                        })).unwrap();
                        if let Err(err) = writer.write_all(&bytes[..]).await {
                            eprintln!("{}", err);
                            return
                        }
                        println!("wrote init message");

                        loop {
                            match rx.recv().await {
                                Some(msg) => {
                                    let bytes = bincode::serialize(&msg).unwrap();
                                    if let Err(_) = writer.write_all(&bytes[..]).await {
                                        break
                                    }
                                },
                                None => break,
                            };
                        }
                    });

                },
                Err(err) => {
                    eprintln!("{}", err);
                    break
                },
            },

            recv_result = tcp_rx.recv() => match recv_result {
                Some((idx, bytes)) if bytes.len() == 0 => {
                    println!("disconnection!");
                    players.remove(idx);
                },
                Some((idx, bytes)) => println!("{:?}: {}", idx, from_utf8(&bytes[..]).unwrap_or("invalid utf-8 :(")),
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
                        game::UdpServerMessage::Init(game::Init { random_bytes }) => {
                            println!("{:?}", random_bytes);
                            if let Some((_, player)) = players.iter_mut().find(|(_, player)| player.random_bytes == random_bytes) {
                                player.udp_addr = Some(socket_addr);
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
