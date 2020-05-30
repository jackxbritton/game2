use generational_arena::Arena;
use nalgebra::Vector2;
use sdl2::event::Event;
use sdl2::gfx::primitives::DrawRenderer;
use sdl2::keyboard::Keycode;
use sdl2::pixels::Color;
use sdl2::rect::Rect;
use std::env::args;
use std::error::Error;
use std::time::Duration;
use tokio::net::{TcpStream, UdpSocket};
use tokio::prelude::*;
use tokio::sync::mpsc::channel;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::time::interval;

use game;

struct Player {
    id: game::Id,
    position: Vector2<f64>,
}

// TODO(jack) Remove the UDP channels and inline it in the main loop.
// It's too slow.

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {

    // Connect with the server.

    let addr = match args().nth(1) {
        Some(addr) => addr,
        None => {
            eprintln!("Usage: {} HOST:PORT", args().nth(0).unwrap());
            return Ok(())
        },
    };

    println!("Connecting to {}...", addr);
    let mut tcp_stream = TcpStream::connect(&addr).await?;

    println!("Waiting for init message...");
    const MAX_PACKET_SIZE_PLUS_ONE: usize = 256;
    let mut buf: [u8; MAX_PACKET_SIZE_PLUS_ONE] = [0; MAX_PACKET_SIZE_PLUS_ONE];
    let num_bytes = tcp_stream.read(&mut buf).await?;

    // Parse player id and random bytes from init message.
    let tcp_init_message: game::TcpClientMessage = bincode::deserialize(&buf[..num_bytes])?;
    let init = match tcp_init_message {
        game::TcpClientMessage::Init(init) => init,
        _ => panic!("Received unexpected message from server!"),
    };

    // Declare players arena.
    let max_players = 16;
    let mut players = Arena::with_capacity(max_players);
    for player in &init.players {
        players.insert(Player {
            id: player.id,
            position: Vector2::new(player.x as f64, player.y as f64),
        });
    }
    let (player_idx, _) = players.iter().find(|(_, p)| p.id == init.id).unwrap();

    // Open the UDP socket and ping the init message back until we get a response.
    let mut udp_socket = UdpSocket::bind("0.0.0.0:0").await?;
    udp_socket.connect(addr).await?;
    let mut ticker = interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                udp_socket.send(bincode::serialize(&game::UdpServerMessage::Init(game::ServerInit { random_bytes: init.random_bytes }))?.as_slice()).await?;
            },
            _ = udp_socket.recv(&mut buf) => break,
        };
    }

    // We're in! Launch separate tasks to read TCP and UDP.
    // TODO(jack) This is super slow. I may want to move away from tokio.

    let (mut internal_tcp_tx, mut tcp_rx) = channel(4);
    let (mut tcp_tx, mut internal_tcp_rx) = channel(4);
    let (mut internal_udp_tx, mut udp_rx) = channel(4);

    let (mut udp_reader, mut udp_writer) = udp_socket.split();
    let (mut tcp_reader, mut tcp_writer) = tcp_stream.into_split();
    tokio::spawn(async move {
        let mut buf: [u8; MAX_PACKET_SIZE_PLUS_ONE] = [0; MAX_PACKET_SIZE_PLUS_ONE];
        loop {
            match tcp_reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(MAX_PACKET_SIZE_PLUS_ONE) => break,
                Ok(num_bytes) => {
                    // Send the message down the internal channel.
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
            };
        }
    });
    tokio::spawn(async move {
        let mut buf: [u8; MAX_PACKET_SIZE_PLUS_ONE] = [0; MAX_PACKET_SIZE_PLUS_ONE];
        loop {
            match udp_reader.recv(&mut buf).await {
                Ok(0) => break,
                Ok(MAX_PACKET_SIZE_PLUS_ONE) => break,
                Ok(num_bytes) => {
                    // Send the message down the internal channel.
                    let udp_message: game::UdpClientMessage = match bincode::deserialize(&buf[..num_bytes]) {
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
            };
        }
    });
    tokio::spawn(async move {
        loop {
            match internal_tcp_rx.recv().await {
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

    // TODO(jack) Load a font.
    let sdl_ttf = sdl2::ttf::init()?;
    let texture_creator = canvas.texture_creator();
    let font = sdl_ttf.load_font("/usr/share/fonts/gsfonts/URWGothic-BookOblique.otf", 128)?;

    let mut event_pump = sdl.event_pump()?;

    let (mut gup, mut gleft, mut gdown, mut gright) = (false, false, false, false);

    let mut last_tick = 0;

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
                    if let Some((_, _)) = players.iter().find(|(_, p)| p.id == id) {
                        // TODO(jack) Initialize the player.
                    } else {
                        players.insert(Player {
                            id,
                            position: Vector2::new(0.0, 0.0),
                        });
                    }
                },
                game::TcpClientMessage::PlayerLeft(game::PlayerLeft { id }) => {
                    if let Some((idx, _)) = players.iter().find(|(_, p)| p.id == id) {
                        println!("dropping player {}", players[idx].id);
                        players.remove(idx);
                    }
                },
                _ => eprintln!("received unknown message from server: {:?}", msg),
            },
            Err(TryRecvError::Empty) => (),
            Err(TryRecvError::Closed) => break,
        }
        loop {
            match udp_rx.try_recv() {
                Ok(msg) => match msg {
                    game::UdpClientMessage::PlayerUpdate(game::PlayerUpdate{ tick, player: game::Player { id, x, y}}) => {
                        last_tick = tick;
                        let (x, y) = (x as f64, y as f64);
                        if let Some((_, player)) = players.iter_mut().find(|(_, p)| p.id == id) {
                            player.position.x = x;
                            player.position.y = y;
                        }
                    },
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Closed) => break 'game,
            }
        }

        // Send input.
        let msg = game::UdpServerMessage::PlayerInput(game::PlayerInput {up: gup, left: gleft, down: gdown, right: gright});
        match udp_writer.send(bincode::serialize(&msg).unwrap().as_slice()).await {
            Ok(_) => (),
            Err(err) => {
                eprintln!("{}", err);
                break
            }
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

        let surface = font
            .render(&format!("hello there\n{}", last_tick))
            .blended(Color::RGB(255, 255, 255))?;
        let texture = texture_creator.create_texture_from_surface(&surface)?;
        canvas.copy(&texture, None, Rect::new(0, 0, surface.width(), surface.height()))?;

        canvas.present();
    }

    Ok(())
}
