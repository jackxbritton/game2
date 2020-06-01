use nalgebra::Vector2;
use serde::{de, Serializer, Serialize, Deserialize};
use std::time::{Duration, SystemTime};

pub const NUM_RANDOM_BYTES: usize = 16;

pub type Id = u8;
pub type Tick = u16;

// TODO(jack) We're serializing f64s as f16s.
// This can obviously be so much better, but it's ok for now.

fn de_from_f16<'de, D>(deserializer: D) -> Result<f64, D::Error>
    where D: de::Deserializer<'de>
{
    let f = <half::f16>::deserialize(deserializer)?;
    Ok(f.to_f64())
}

fn se_to_f16<S>(f: &f64, serializer: S) -> Result<S::Ok, S::Error>
    where S: Serializer
{
    half::f16::from_f64(*f).serialize(serializer)
}

fn de_from_vector2_f16<'de, D>(deserializer: D) -> Result<Vector2<f64>, D::Error>
    where D: de::Deserializer<'de>
{
    let v = <Vector2<half::f16>>::deserialize(deserializer)?;
    Ok(Vector2::new(v.x.to_f64(), v.y.to_f64()))
}

fn se_to_vector2_f16<S>(f: &Vector2<f64>, serializer: S) -> Result<S::Ok, S::Error>
    where S: Serializer
{
    let v = Vector2::new(half::f16::from_f64(f.x), half::f16::from_f64(f.y));
    v.serialize(serializer)
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
pub struct Player {
    pub id: Id,
    #[serde(serialize_with = "se_to_f16", deserialize_with = "de_from_f16")]
    pub radius: f64,
    #[serde(serialize_with = "se_to_vector2_f16", deserialize_with = "de_from_vector2_f16")]
    pub position: Vector2<f64>,
    #[serde(serialize_with = "se_to_vector2_f16", deserialize_with = "de_from_vector2_f16")]
    pub velocity: Vector2<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldUpdate {
    pub tick: Tick,
    pub players: Vec<Player>,
}

// TODO(jack) ClientInit should be split into a handshake message and a game init message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInit {
    pub id: Id,
    pub random_bytes: [u8; NUM_RANDOM_BYTES],
    pub update: WorldUpdate,
    pub tick_period: Duration,
    pub tick_zero: SystemTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInit {
    pub random_bytes: [u8; NUM_RANDOM_BYTES],
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
pub struct PlayerInput {
    pub tick: Tick,
    pub up: bool,
    pub left: bool,
    pub down: bool,
    pub right: bool,
    #[serde(serialize_with = "se_to_f16", deserialize_with = "de_from_f16")]
    pub angle: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TcpClientMessage {
    Init(ClientInit),
    PlayerJoined(Id),  // Could add more initialization in the future.
    PlayerLeft(Id),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TcpServerMessage {
    Test(u8),
}

#[derive(Debug, Serialize, Deserialize)]
pub enum UdpClientMessage {
    WorldUpdate(WorldUpdate),
}

#[derive(Debug, Serialize, Deserialize)]
pub enum UdpServerMessage {
    Init(ServerInit),
    PlayerInput(Vec<PlayerInput>),
}
