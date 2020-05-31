use sdl2::event::Event;
use sdl2::gfx::primitives::DrawRenderer;
use sdl2::keyboard::Keycode;
use sdl2::pixels::Color;
use sdl2::rect::Rect;
use std::env::args;
use std::error::Error;
use std::time::Duration;
use std::time::SystemTime;
use tokio::net::{TcpStream, UdpSocket};
use tokio::prelude::*;
use tokio::sync::mpsc::channel;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::time::interval;

use game;

struct World {
    updates: [game::WorldUpdate; 2],
}

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
    let mut world = World {
        updates: [
            game::WorldUpdate { tick: 0, players: vec![] },
            game::WorldUpdate { tick: 0, players: vec![] },
        ],
    };
    let player_id = init.id;

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
    tcp_tx.try_send(&game::TcpServerMessage::Test(4))?;

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

    // Load a font.
    let sdl_ttf = sdl2::ttf::init()?;
    let texture_creator = canvas.texture_creator();
    let font = sdl_ttf.load_font("/usr/share/fonts/gsfonts/URWGothic-BookOblique.otf", 128)?;

    let mut event_pump = sdl.event_pump()?;

    let (mut gup, mut gleft, mut gdown, mut gright) = (false, false, false, false);

    let tick_period = 1.0 / init.tick_rate as f64;
    let mut current_time = SystemTime::now();

    let duration_since_tick_zero = current_time
        .duration_since(init.tick_zero)?;
    let mut tick_dt_counter = duration_since_tick_zero.as_secs_f64() % tick_period;

    let mut tick = duration_since_tick_zero.as_nanos() / (tick_period * 1e9) as u128;

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
                game::TcpClientMessage::PlayerJoined(id) => {
                    println!("player joined: {}", id);
                },
                game::TcpClientMessage::PlayerLeft(id) => {
                    println!("player left: {}", id);
                },
                _ => eprintln!("received unknown message from server: {:?}", msg),
            },
            Err(TryRecvError::Empty) => (),
            Err(TryRecvError::Closed) => break,
        }
        loop {
            match udp_rx.try_recv() {
                Ok(msg) => match msg {
                    game::UdpClientMessage::WorldUpdate(update) => {

                        // Sort the update into the world updates.
                        let mut updates = Vec::with_capacity(world.updates.len() + 1);
                        updates.push(update);
                        updates.extend_from_slice(&world.updates);
                        updates.sort_unstable_by_key(|w| w.tick);
                        world.updates.clone_from_slice(&updates[1..]);

                    },
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Closed) => break 'game,
            }
        }

        let now = SystemTime::now();
        let dt = now.duration_since(current_time)?.as_secs_f64();
        current_time = now;

        tick_dt_counter += dt;
        if tick_dt_counter >= tick_period {
            tick_dt_counter %= tick_period;
            tick += 1;
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

        let num_ticks = world.updates[1].tick - world.updates[0].tick;
        let mix_unclamped = ((tick as i32 - world.updates[0].tick as i32 - 1) as f64 + tick_dt_counter / tick_period) / num_ticks as f64;
        let mix = if mix_unclamped < 0.0 { 0.0 } else if mix_unclamped > 1.0 { 1.0 } else { mix_unclamped };

        for player in &world.updates[1].players {
            let color = if player.id == player_id {
                Color::RGB(255, 255, 255)
            } else {
                Color::RGB(255, 0, 0)
            };
            let previous_player = match world.updates[0].players.iter().find(|p| p.id == player.id) {
                Some(p) => p,
                None => player,
            };

            let position = mix * player.position + (1.0 - mix) * previous_player.position;
            let radius = mix * player.radius + (1.0 - mix) * previous_player.radius;

            let scale = 40.0;
            let position = position * scale;
            let radius = radius * scale;
            canvas.filled_circle(
                position.x as i16,
                position.y as i16,
                radius as i16,
                color,
            )?;
        }

        let surface = font
            .render(&format!(
                "{} {}",
                world.updates[1].tick,
                tick, 
            ))
            .blended(Color::RGB(255, 255, 255))?;
        let texture = texture_creator.create_texture_from_surface(&surface)?;
        canvas.copy(&texture, None, Rect::new(0, 0, surface.width(), surface.height()))?;

        canvas.present();
    }

    Ok(())
}
