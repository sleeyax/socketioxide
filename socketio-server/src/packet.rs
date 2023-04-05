use itertools::Itertools;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;

use crate::errors::Error;

/// The socket.io packet type.
/// Each packet has a type and a namespace
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Packet<T> {
    pub inner: PacketData<T>,
    pub ns: String,
}

impl Packet<ConnectPacket> {
    pub fn connect(ns: String, sid: i64) -> Self {
        Self {
            inner: PacketData::Connect(Some(ConnectPacket {
                sid: sid.to_string(),
            })),
            ns,
        }
    }
}

impl Packet<()> {
    pub fn invalid_namespace(ns: String) -> Self {
        Self {
            inner: PacketData::ConnectError(ConnectErrorPacket {
                message: "Invalid namespace".to_string(),
            }),
            ns,
        }
    }
}

impl<T> Packet<T> {
    pub fn event(ns: String, e: String, data: T) -> Self {
        Self {
            inner: PacketData::Event(e, data),
            ns,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PacketData<T> {
    Connect(Option<T>),
    Disconnect,
    Event(String, T),
    Ack(i64),
    ConnectError(ConnectErrorPacket),
    BinaryEvent(String, T, Vec<Vec<u8>>),
    BinaryAck(T, Vec<Vec<u8>>),
}

impl<T> PacketData<T> {
    fn index(&self) -> u8 {
        match self {
            PacketData::Connect(_) => 0,
            PacketData::Disconnect => 1,
            PacketData::Event(_, _) => 2,
            PacketData::Ack(_) => 3,
            PacketData::ConnectError(_) => 4,
            PacketData::BinaryEvent(_, _, _) => 5,
            PacketData::BinaryAck(_, _) => 6,
        }
    }
}

impl<T> TryInto<String> for Packet<T>
where
    T: Serialize,
{
    type Error = Error;

    fn try_into(self) -> Result<String, Self::Error> {
        let mut res = self.inner.index().to_string();
        if !self.ns.is_empty() && self.ns != "/" {
            res.push_str(&format!("{},", self.ns));
        }

        match self.inner {
            PacketData::Connect(None) => (),
            PacketData::Connect(Some(data)) => res.push_str(&serde_json::to_string(&data)?),
            PacketData::Disconnect => (),
            PacketData::Event(event, data) => res.push_str(&serde_json::to_string(&(event, data))?),
            PacketData::Ack(_) => todo!(),
            PacketData::ConnectError(data) => res.push_str(&serde_json::to_string(&data)?),
            PacketData::BinaryEvent(_, _, _) => todo!(),
            PacketData::BinaryAck(_, _) => todo!(),
        };
        Ok(res)
    }
}

/// Deserialize an event packet from a string, formated as:
/// ```text
/// ["<event name>", ...<JSON-stringified payload without binary>]
/// ```
fn deserialize_event_packet(data: &str) -> Result<(String, Value), Error> {
    debug!("Deserializing event packet: {:?}", data);
    let packet = match serde_json::from_str::<Value>(data)? {
        Value::Array(packet) => packet,
        _ => return Err(Error::InvalidEventName),
    };

    let event = packet
        .get(0)
        .ok_or(Error::InvalidEventName)?
        .as_str()
        .ok_or(Error::InvalidEventName)?
        .to_string();
    let payload = Value::from_iter(packet.into_iter().skip(1));
    Ok((event, payload))
}

fn deserialize_packet<T: DeserializeOwned>(data: &str) -> Result<Option<T>, Error> {
    debug!("Deserializing packet: {:?}", data);
    let packet = if data.is_empty() {
        None
    } else {
        Some(serde_json::from_str(&data)?)
    };
    Ok(packet)
}

/// Deserialize a packet from a string
/// The string should be in the format of:
/// ```text
/// <packet type>[<# of binary attachments>-][<namespace>,][<acknowledgment id>][JSON-stringified payload without binary]
/// + binary attachments extracted
/// ```
impl TryFrom<String> for Packet<Value> {
    type Error = Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let mut chars = value.chars();
        let index = chars.next().ok_or(Error::InvalidPacketType)?;
        //TODO: attachments
        let attachments: u32 = chars
            .take_while_ref(|c| *c != '-' && c.is_digit(10))
            .collect::<String>()
            .parse()
            .unwrap_or(0);

        // If there are attachments, skip the `-` separator
        if attachments > 0 {
            chars.next();
        }
        let mut ns: String = chars
            .take_while_ref(|c| *c != ',' && *c != '{' && *c != '[' && !c.is_digit(10))
            .collect();

        // If there is a namespace, skip the `,` separator
        if !ns.is_empty() {
            chars.next();
        }
        //TODO: improve ?
        if !ns.starts_with("/") {
            ns.insert(0, '/');
        }
        //TODO: ack
        let _ack: Option<i64> = chars
            .take_while_ref(|c| c.is_digit(10))
            .collect::<String>()
            .parse()
            .ok();

        let data = chars.as_str();
        let inner = match index {
            '0' => PacketData::Connect(deserialize_packet(&data)?),
            '1' => PacketData::Disconnect,
            '2' => {
                let (event, payload) = deserialize_event_packet(&data)?;
                PacketData::Event(event, payload)
            }
            '3' => todo!(),
            '4' => PacketData::ConnectError(
                deserialize_packet(&data)?.ok_or(Error::InvalidPacketType)?,
            ),
            '5' => todo!(),
            '6' => todo!(),
            _ => return Err(Error::InvalidPacketType),
        };

        Ok(Self { inner, ns })
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Placeholder {
    #[serde(rename = "_placeholder")]
    placeholder: bool,
    num: u32,
}

/// Connect packet sent by the client
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectPacket {
    sid: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectErrorPacket {
    message: String,
}
