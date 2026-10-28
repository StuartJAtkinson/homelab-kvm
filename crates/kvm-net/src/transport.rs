//! Encrypted framing over a byte stream.
//!
//! A connection starts with a Noise `XX` handshake: each side proves its
//! static X25519 key, and both derive the same handshake hash, which becomes
//! the 6-digit pairing code users compare. After that every [`Message`] is
//! JSON, split into chunks of at most one Noise message (64 KiB):
//!
//! ```text
//! packet  = u16 BE length || ciphertext
//! payload = flag (1 = more chunks follow, 0 = last) || chunk
//! ```

use std::io;
use std::sync::Arc;

use kvm_core::protocol::Message;
use snow::{Builder, HandshakeState, StatelessTransportState};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
/// Binds the handshake to this application, so keys can't be replayed into
/// some other Noise protocol.
const PROLOGUE: &[u8] = b"homelab-kvm";
const NOISE_MAX: usize = 65535;
const TAG: usize = 16;
const CHUNK: usize = NOISE_MAX - TAG - 1;
/// Layouts are a few KB; anything this large is a bug or an attack.
pub const MAX_MESSAGE: usize = 1 << 20;

/// This machine's long-term Noise key pair.
#[derive(Clone)]
pub struct Identity {
    pub private: [u8; 32],
    pub public: [u8; 32],
}

impl Identity {
    pub fn generate() -> Self {
        let kp = builder().generate_keypair().expect("X25519 key generation");
        Self {
            private: kp.private.try_into().expect("32-byte private key"),
            public: kp.public.try_into().expect("32-byte public key"),
        }
    }

    pub fn public_hex(&self) -> String {
        hex::encode(self.public)
    }
}

fn builder() -> Builder<'static> {
    Builder::new(NOISE_PARAMS.parse().expect("valid noise params"))
}

fn noise_err(e: snow::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

/// Outcome of a successful handshake.
pub struct Session<R, W> {
    pub reader: SecureReader<R>,
    pub writer: SecureWriter<W>,
    /// The peer's proven static public key.
    pub remote_key: [u8; 32],
    /// Pairing code, identical on both ends; e.g. `"042 917"`.
    pub code: String,
}

pub async fn handshake<R, W>(mut r: R, mut w: W, me: &Identity, initiator: bool) -> io::Result<Session<R, W>>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let b = builder().local_private_key(&me.private).map_err(noise_err)?;
    let b = b.prologue(PROLOGUE).map_err(noise_err)?;
    let mut hs = if initiator { b.build_initiator() } else { b.build_responder() }.map_err(noise_err)?;

    // XX is three messages: -> e; <- e, ee, s, es; -> s, se.
    let mut buf = vec![0u8; NOISE_MAX];
    let mut scratch = vec![0u8; NOISE_MAX];
    while !hs.is_handshake_finished() {
        if hs.is_my_turn() {
            let n = hs.write_message(&[], &mut buf).map_err(noise_err)?;
            write_packet(&mut w, &buf[..n]).await?;
        } else {
            let n = read_packet(&mut r, &mut buf).await?.ok_or(io::ErrorKind::UnexpectedEof)?;
            hs.read_message(&buf[..n], &mut scratch).map_err(noise_err)?;
        }
    }

    let remote_key: [u8; 32] = hs
        .get_remote_static()
        .and_then(|k| k.try_into().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no remote static key"))?;
    let code = pairing_code(&hs);
    let cipher = Arc::new(hs.into_stateless_transport_mode().map_err(noise_err)?);
    Ok(Session {
        reader: SecureReader { r, cipher: cipher.clone(), nonce: 0 },
        writer: SecureWriter { w, cipher, nonce: 0 },
        remote_key,
        code,
    })
}

fn pairing_code(hs: &HandshakeState) -> String {
    let h = hs.get_handshake_hash();
    let n = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) % 1_000_000;
    format!("{:03} {:03}", n / 1000, n % 1000)
}

async fn write_packet<W: AsyncWrite + Unpin>(w: &mut W, data: &[u8]) -> io::Result<()> {
    w.write_u16(data.len() as u16).await?;
    w.write_all(data).await
}

/// `Ok(None)` on a clean close at a packet boundary.
async fn read_packet<R: AsyncRead + Unpin>(r: &mut R, buf: &mut [u8]) -> io::Result<Option<usize>> {
    let len = match r.read_u16().await {
        Ok(n) => n as usize,
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    };
    r.read_exact(&mut buf[..len]).await?;
    Ok(Some(len))
}

pub struct SecureWriter<W> {
    w: W,
    cipher: Arc<StatelessTransportState>,
    nonce: u64,
}

impl<W: AsyncWrite + Unpin> SecureWriter<W> {
    pub async fn send(&mut self, msg: &Message) -> io::Result<()> {
        let json = serde_json::to_vec(msg)?;
        if json.len() > MAX_MESSAGE {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "message too large"));
        }
        let mut plain = Vec::with_capacity(CHUNK + 1);
        let mut out = vec![0u8; NOISE_MAX];
        let mut chunks = json.chunks(CHUNK).peekable();
        while let Some(chunk) = chunks.next() {
            plain.clear();
            plain.push(u8::from(chunks.peek().is_some()));
            plain.extend_from_slice(chunk);
            let n = self.cipher.write_message(self.nonce, &plain, &mut out).map_err(noise_err)?;
            self.nonce += 1;
            write_packet(&mut self.w, &out[..n]).await?;
        }
        self.w.flush().await
    }
}

pub struct SecureReader<R> {
    r: R,
    cipher: Arc<StatelessTransportState>,
    nonce: u64,
}

impl<R: AsyncRead + Unpin> SecureReader<R> {
    /// `Ok(None)` on a clean close between messages.
    pub async fn recv(&mut self) -> io::Result<Option<Message>> {
        let mut packet = vec![0u8; NOISE_MAX];
        let mut plain = vec![0u8; NOISE_MAX];
        let mut json = Vec::new();
        loop {
            let Some(n) = read_packet(&mut self.r, &mut packet).await? else {
                return if json.is_empty() { Ok(None) } else { Err(io::ErrorKind::UnexpectedEof.into()) };
            };
            let m = self.cipher.read_message(self.nonce, &packet[..n], &mut plain).map_err(noise_err)?;
            self.nonce += 1;
            let (&more, chunk) = plain[..m]
                .split_first()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "empty chunk"))?;
            json.extend_from_slice(chunk);
            if json.len() > MAX_MESSAGE {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "message too large"));
            }
            if more == 0 {
                return Ok(Some(serde_json::from_slice(&json)?));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kvm_core::protocol::InputEvent;
    use kvm_core::{NodeId, NodeInfo, Os};
    use tokio::io::{duplex, split, DuplexStream, ReadHalf, WriteHalf};

    type S = Session<ReadHalf<DuplexStream>, WriteHalf<DuplexStream>>;

    async fn connect(a: &Identity, b: &Identity) -> (S, S) {
        let (x, y) = duplex(1 << 16);
        let ((xr, xw), (yr, yw)) = (split(x), split(y));
        let (sa, sb) = tokio::join!(handshake(xr, xw, a, true), handshake(yr, yw, b, false));
        (sa.unwrap(), sb.unwrap())
    }

    #[tokio::test]
    async fn handshake_proves_keys_and_agrees_on_code() {
        let (a, b) = (Identity::generate(), Identity::generate());
        let (sa, sb) = connect(&a, &b).await;
        assert_eq!(sa.remote_key, b.public);
        assert_eq!(sb.remote_key, a.public);
        assert_eq!(sa.code, sb.code);
        assert_eq!(sa.code.len(), 7);
        // A fresh session gets fresh ephemeral keys, so a different code.
        let (sc, _) = connect(&a, &b).await;
        assert_ne!(sa.code, sc.code);
    }

    #[tokio::test]
    async fn small_and_multi_chunk_messages_round_trip() {
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut sa, mut sb) = connect(&a, &b).await;

        let small = Message::Input(InputEvent::MouseMove { dx: 3, dy: -2 });
        let big = Message::NodeUpdate(NodeInfo {
            id: NodeId::new(),
            name: "x".repeat(200_000),
            os: Os::Linux,
            displays: vec![],
            portable: false,
            key: String::new(),
        });
        let send = async {
            sa.writer.send(&small).await.unwrap();
            sa.writer.send(&big).await.unwrap();
            // Both halves of a split duplex must go before the other end sees EOF.
            drop((sa.writer, sa.reader));
        };
        let recv = async {
            assert_eq!(sb.reader.recv().await.unwrap(), Some(small.clone()));
            assert_eq!(sb.reader.recv().await.unwrap(), Some(big.clone()));
            assert_eq!(sb.reader.recv().await.unwrap(), None);
        };
        tokio::join!(send, recv);
    }

    #[tokio::test]
    async fn tampered_packets_are_rejected() {
        let (a, b) = (Identity::generate(), Identity::generate());
        let (mut sa, mut sb) = connect(&a, &b).await;
        write_packet(&mut sa.writer.w, &[0x42; 40]).await.unwrap();
        assert_eq!(sb.reader.recv().await.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn garbage_or_hangup_fails_handshake() {
        let a = Identity::generate();
        let (x, y) = duplex(1 << 16);
        let (xr, xw) = split(x);
        let (yr, mut yw) = split(y);
        let attacker = async move {
            write_packet(&mut yw, &[7; 48]).await.unwrap();
            // Hang up: the responder must fail, not wait forever.
            drop((yr, yw));
        };
        let (res, _) = tokio::join!(handshake(xr, xw, &a, false), attacker);
        assert!(res.is_err());
    }
}
