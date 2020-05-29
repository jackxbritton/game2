use serde::{Serialize, Deserialize};

pub const NUM_RANDOM_BYTES: usize = 16;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Init {
    pub random_bytes: [u8; NUM_RANDOM_BYTES],
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PlayerUpdate {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PlayerInput {
    pub up: bool,
    pub left: bool,
    pub down: bool,
    pub right: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TcpClientMessage {
    Init(Init),
}
#[derive(Debug, Serialize, Deserialize)]
pub enum TcpServerMessage {
    Test(&'static str),
}

#[derive(Debug, Serialize, Deserialize)]
pub enum UdpClientMessage {
    PlayerUpdate(PlayerUpdate),
}

#[derive(Debug, Serialize, Deserialize)]
pub enum UdpServerMessage {
    Init(Init),
    PlayerInput(PlayerInput),
}
