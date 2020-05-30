use serde::{Serialize, Deserialize};

pub const NUM_RANDOM_BYTES: usize = 16;

pub type Id = u8;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInit {
    pub id: Id,
    pub random_bytes: [u8; NUM_RANDOM_BYTES],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInit {
    pub random_bytes: [u8; NUM_RANDOM_BYTES],
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PlayerUpdate {
    pub id: Id,
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
pub struct PlayerJoined {
    pub id: Id,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerLeft {
    pub id: Id,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TcpClientMessage {
    Init(ClientInit),
    PlayerJoined(PlayerJoined),
    PlayerLeft(PlayerLeft),
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
    Init(ServerInit),
    PlayerInput(PlayerInput),
}
