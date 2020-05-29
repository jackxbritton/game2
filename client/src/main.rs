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

use game;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // TODO(jack)
    // Connect with the server.

    let addr = "127.0.0.1:5000";
    println!("Connecting to {}...", addr);
    let mut tcp_stream = TcpStream::connect(addr).await?;

    println!("Waiting for init message...");
    const MAX_PACKET_SIZE_PLUS_ONE: usize = 256;
    let mut buf: [u8; MAX_PACKET_SIZE_PLUS_ONE] = [0; MAX_PACKET_SIZE_PLUS_ONE];
    let num_bytes = tcp_stream.read(&mut buf).await?;

    let mut buf2: [u8; MAX_PACKET_SIZE_PLUS_ONE] = [0; MAX_PACKET_SIZE_PLUS_ONE];  // TODO(jack) Hilarious.

    // Parse random bytes from init message.
    let tcp_init_message: game::TcpClientMessage = bincode::deserialize(&buf[..num_bytes])?;
    let random_bytes = match tcp_init_message {
        game::TcpClientMessage::Init(game::Init { random_bytes}) => random_bytes,
    };

    // Open the UDP socket and ping the init message back until we get a response.
    let mut udp_socket = UdpSocket::bind("127.0.0.1:5001").await?;
    udp_socket.connect(addr).await?;
    let mut ticker = interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                udp_socket.send(bincode::serialize(&game::UdpServerMessage::Init(game::Init { random_bytes }))?.as_slice()).await?;
            },
            _ = udp_socket.recv(&mut buf) => break,
        };
    }

    // We're in! Launch separate tasks to read TCP and UDP.

    let (mut internal_tcp_tx, mut tcp_rx) = channel(4);
    let (mut tcp_tx, mut internal_tcp_rx) = channel(4);
    let (mut internal_udp_tx, mut udp_rx) = channel(4);
    let (mut udp_tx, mut internal_udp_rx) = channel(4);

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
                        let tcp_message: game::TcpClientMessage = match bincode::deserialize(&buf[..num_bytes]) {
                            Ok(msg) => msg,
                            Err(err) => {
                                eprintln!("{}", err);
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
    let window = sdl_video.window("game", 600, 480).build()?;
    let mut canvas = window.into_canvas().accelerated().present_vsync().build()?;

    let mut event_pump = sdl.event_pump()?;

    let (mut gx, mut gy) = (0.0, 0.0);
    let (mut gup, mut gleft, mut gdown, mut gright) = (false, false, false, false);

    'game: loop {
        for event in event_pump.poll_iter() {
            match event {
                Event::Quit { .. } => break 'game,
                Event::KeyDown {
                    keycode: Some(Keycode::W),
                    ..
                } => gup = true,
                Event::KeyUp {
                    keycode: Some(Keycode::W),
                    ..
                } => gup = false,
                Event::KeyDown {
                    keycode: Some(Keycode::A),
                    ..
                } => gleft = true,
                Event::KeyUp {
                    keycode: Some(Keycode::A),
                    ..
                } => gleft = false,
                Event::KeyDown {
                    keycode: Some(Keycode::S),
                    ..
                } => gdown = true,
                Event::KeyUp {
                    keycode: Some(Keycode::S),
                    ..
                } => gdown = false,
                Event::KeyDown {
                    keycode: Some(Keycode::D),
                    ..
                } => gright = true,
                Event::KeyUp {
                    keycode: Some(Keycode::D),
                    ..
                } => gright = false,
                _ => (),
            };
        }

        // Read TCP and UDP messages.
        match tcp_rx.try_recv() {
            Ok(_) => println!("received a tcp message!"),
            Err(TryRecvError::Empty) => (),
            Err(TryRecvError::Closed) => break,
        }
        match udp_rx.try_recv() {
            Ok(msg) => match msg {
                game::UdpClientMessage::PlayerUpdate(game::PlayerUpdate { x, y}) => {
                    gx = x;
                    gy = y;
                },
            },
            Err(TryRecvError::Empty) => (),
            Err(TryRecvError::Closed) => break,
        }

        // Also write for fun.
        let msg = game::UdpServerMessage::PlayerInput(game::PlayerInput {up: gup.clone(), left: gleft.clone(), down: gdown.clone(), right: gright.clone()});
        match udp_tx.try_send(msg) {
            Ok(_) => (),
            Err(TrySendError::Closed(_)) => break,
            Err(TrySendError::Full(_)) => eprintln!("tcp channel is full!"),
        };

        canvas.set_draw_color(Color::RGB(0, 0, 0));
        canvas.clear();

        canvas.filled_circle((gx * 800.0) as i16, (gy * 800.0) as i16, 40, Color::RGB(255, 255, 255))?;

        canvas.present();
    }

    Ok(())
}
