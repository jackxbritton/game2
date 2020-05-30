use generational_arena::Arena;
use nalgebra::Vector2;
use sdl2::event::Event;
use sdl2::gfx::primitives::DrawRenderer;
use sdl2::keyboard::Keycode;
use sdl2::pixels::Color;
use std::error::Error;
use std::time::Duration;
use tokio::net::{TcpStream, UdpSocket};
use tokio::prelude::*;
use tokio::sync::mpsc::channel;
use tokio::sync::mpsc::error::{TryRecvError, TrySendError};
use tokio::time::interval;
use std::env;

use game;

struct Player {
    id: game::Id,
    position: Vector2<f64>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {

    // Connect with the server.

    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        eprintln!("Usage: {} HOST:PORT", args[0]);
        return Ok(())
    }
    let addr = &args[1];

    println!("Connecting to {}...", addr);
    let mut tcp_stream = TcpStream::connect(addr).await?;

    println!("Waiting for init message...");
    const MAX_PACKET_SIZE_PLUS_ONE: usize = 256;
    let mut buf: [u8; MAX_PACKET_SIZE_PLUS_ONE] = [0; MAX_PACKET_SIZE_PLUS_ONE];
    let num_bytes = tcp_stream.read(&mut buf).await?;

    // Parse player id and random bytes from init message.
    let tcp_init_message: game::TcpClientMessage = bincode::deserialize(&buf[..num_bytes])?;
    let (id, random_bytes) = match tcp_init_message {
        game::TcpClientMessage::Init(game::ClientInit { id, random_bytes}) => (id, random_bytes),
        _ => panic!("Received unexpected message from server!"),
    };

    // Declare players arena.
    let max_players = 16;
    let mut players = Arena::with_capacity(max_players);
    let player_idx = players.insert(Player {
        id,
        position: Vector2::new(0.0, 0.0),
    });

    // Open the UDP socket and ping the init message back until we get a response.
    let mut udp_socket = UdpSocket::bind("0.0.0.0:0").await?;
    udp_socket.connect(addr).await?;
    let mut ticker = interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                udp_socket.send(bincode::serialize(&game::UdpServerMessage::Init(game::ServerInit { random_bytes }))?.as_slice()).await?;
            },
            _ = udp_socket.recv(&mut buf) => break,
        };
    }

    // We're in! Launch separate tasks to read TCP and UDP.

    let (mut internal_tcp_tx, mut tcp_rx) = channel(4);
    let (mut tcp_tx, mut internal_tcp_rx) = channel(4);
    let (mut internal_udp_tx, mut udp_rx) = channel(4);
    let (mut udp_tx, mut internal_udp_rx) = channel(4);

    let mut buf2: [u8; MAX_PACKET_SIZE_PLUS_ONE] = [0; MAX_PACKET_SIZE_PLUS_ONE];  // TODO(jack) Hilarious.

    let (mut udp_reader, mut udp_writer) = udp_socket.split();
    tokio::spawn(async move {
        let (mut tcp_reader, mut tcp_writer) = tcp_stream.split();
        loop {
            tokio::select! {
                result = tcp_reader.read(&mut buf) => match result {
                    Ok(0) => break,
                    Ok(MAX_PACKET_SIZE_PLUS_ONE) => break,
                    Ok(num_bytes) => {
                        // Send the message down the internal channel.
                        println!("deserializing tcp message with length {}", num_bytes);
                        println!("{:?}", &buf[..num_bytes]);
                        let tcp_message: game::TcpClientMessage = match bincode::deserialize(&buf[..num_bytes]) {
                            Ok(msg) => msg,
                            Err(err) => {
                                eprintln!("failed to deserialize TCP message: {}", err);
                                break
                            }
                        };
                        match internal_tcp_tx.send(tcp_message).await {
                            Ok(_) => (),
                            Err(err) => {
                                eprintln!("{}", err);
                                break
                            },
                        };
                    },
                    Err(err) => {
                        eprintln!("{}", err);
                        break
                    },
                },
                result = udp_reader.recv(&mut buf2) => match result {
                    Ok(0) => break,
                    Ok(MAX_PACKET_SIZE_PLUS_ONE) => break,
                    Ok(num_bytes) => {
                        // Send the message down the internal channel.
                        let udp_message: game::UdpClientMessage = match bincode::deserialize(&buf2[..num_bytes]) {
                            Ok(msg) => msg,
                            Err(err) => {
                                eprintln!("{}", err);
                                break
                            }
                        };
                        match internal_udp_tx.send(udp_message).await {
                            Ok(_) => (),
                            Err(err) => {
                                eprintln!("{}", err);
                                break
                            },
                        };
                    },
                    Err(err) => {
                        eprintln!("{}", err);
                        break
                    },
                },
                option = internal_tcp_rx.recv() => match option {
                    Some(msg) => {
                        match tcp_writer.write(match bincode::serialize(msg) {
                            Ok(msg) => msg,
                            Err(err) => {
                                eprintln!("{}", err);
                                break
                            },
                        }.as_slice()).await {
                            Ok(_) => (),
                            Err(err) => {
                                eprintln!("{}", err);
                                break
                            }
                        };
                    },
                    None => break,
                },
                option = internal_udp_rx.recv() => match option {
                    Some(msg) => {
                        match udp_writer.send(match bincode::serialize(&msg) {
                            Ok(msg) => msg,
                            Err(err) => {
                                eprintln!("{}", err);
                                break
                            },
                        }.as_slice()).await {
                            Ok(_) => (),
                            Err(err) => {
                                eprintln!("{}", err);
                                break
                            }
                        };
                    },
                    None => break,
                },
            };
        }
    });

    // Write a dummy message to the server.
    tcp_tx.try_send(&game::TcpServerMessage::Test("hi"))?;

    let sdl = sdl2::init()?;
    let sdl_video = sdl.video()?;
    let window = sdl_video
        .window("game", 600, 480)
        .resizable()
        .build()?;
    let mut canvas = window
        .into_canvas()
        .accelerated()
        .present_vsync()
        .build()?;

    let mut event_pump = sdl.event_pump()?;

    let (mut gup, mut gleft, mut gdown, mut gright) = (false, false, false, false);

    'game: loop {
        for event in event_pump.poll_iter() {

            let keycode_and_on = match event {
                Event::KeyDown { keycode: Some(keycode), .. } => Some((keycode, true)),
                Event::KeyUp { keycode: Some(keycode), .. } => Some((keycode, false)),
                _ => None,
            };

            match (event, keycode_and_on) {
                (_, Some((Keycode::W, on))) => gup = on,
                (_, Some((Keycode::A, on))) => gleft = on,
                (_, Some((Keycode::S, on))) => gdown = on,
                (_, Some((Keycode::D, on))) => gright = on,
                (Event::Quit {..}, _) => break 'game,
                _ => (),
            };

        }

        // Read TCP and UDP messages.
        match tcp_rx.try_recv() {
            Ok(msg) => match msg {
                game::TcpClientMessage::PlayerJoined(game::PlayerJoined { id }) => {
                    if let Some((_, _)) = players.iter_mut().find(|(_, p)| p.id == id) {
                        // TODO(jack) Eventually initialize the player.
                    } else {
                        players.insert(Player {
                            id,
                            position: Vector2::new(0.0, 0.0),
                        });
                    }
                },
                _ => eprintln!("received unknown message from server: {:?}", msg),
            },
            Err(TryRecvError::Empty) => (),
            Err(TryRecvError::Closed) => break,
        }
        match udp_rx.try_recv() {
            Ok(msg) => match msg {
                game::UdpClientMessage::PlayerUpdate(game::PlayerUpdate { id, x, y}) => {
                    let (x, y) = (x as f64, y as f64);
                    if let Some((_, player)) = players.iter_mut().find(|(_, p)| p.id == id) {
                        player.position.x = x;
                        player.position.y = y;
                    } else {
                        players.insert(Player {
                            id,
                            position: Vector2::new(x, y),
                        });
                    }
                },
            },
            Err(TryRecvError::Empty) => (),
            Err(TryRecvError::Closed) => break,
        }

        // Send input.
        let msg = game::UdpServerMessage::PlayerInput(game::PlayerInput {up: gup.clone(), left: gleft.clone(), down: gdown.clone(), right: gright.clone()});
        match udp_tx.try_send(msg) {
            Ok(_) => (),
            Err(TrySendError::Closed(_)) => break,
            Err(TrySendError::Full(_)) => eprintln!("udp channel is full!"),
        };

        canvas.set_draw_color(Color::RGB(0, 0, 0));
        canvas.clear();

        for (idx, player) in players.iter() {
            let color = if idx == player_idx {
                Color::RGB(255, 255, 255)
            } else {
                Color::RGB(255, 0, 0)
            };
            canvas.filled_circle((player.position.x * 800.0) as i16, (player.position.y * 800.0) as i16, 40, color)?;
        }

        canvas.present();
    }

    Ok(())
}
