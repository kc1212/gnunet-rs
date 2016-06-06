use std::io;
use service::{self, ReadMessageError, MessageHeader, MessageTrait};
use hello::HelloDeserializeError;
use Hello;
use Cfg;
use ll;

pub struct TransportService {
  //service_reader: ServiceReader,
  //service_writer: ServiceWriter,
  our_hello:      Hello,
}

error_def! TransportServiceInitError {
  NonHelloMessage { ty: u16 }
    => "Expected a HELLO message from the service but received a different message type" ("Received message type {} instead.", ty),
  Io { #[from] cause: io::Error }
    => "There was an I/O error communicating with the service" ("Error: {}", cause),
  ReadMessage { #[from] cause: ReadMessageError }
    => "Failed to receive a message from the service" ("Reason: {}", cause),
  Connect { #[from] cause: service::ConnectError } 
    => "Failed to connect to the transport service" ("Reason: {}", cause),
  HelloDeserialize { #[from] cause: HelloDeserializeError }
    => "Failed to serialize the hello message from the service" ("Reason {}", cause),
}

impl TransportService {
  pub fn init(cfg: &Cfg) -> Result<TransportService, TransportServiceInitError> {
    let (mut sr, mut sw) = try!(service::connect(cfg, "transport"));
    let msg = StartMessage::new(0,
                        ll::Struct_GNUNET_PeerIdentity {
                            public_key: ll::Struct_GNUNET_CRYPTO_EddsaPublicKey {
                                q_y: [0; 32],
                            }
                        });
    let mw = sw.write_message(msg);
    try!(mw.send());
    let (ty, mut mr) = try!(sr.read_message());
    if ty != ll::GNUNET_MESSAGE_TYPE_HELLO {
      return Err(TransportServiceInitError::NonHelloMessage { ty: ty });
    };
    let hello = try!(Hello::deserialize(&mut mr));
    Ok(TransportService {
      //service_reader: sr,
      //service_writer: sw,
      our_hello:      hello,
    })
  }
}

pub fn self_hello(cfg: &Cfg) -> Result<Hello, TransportServiceInitError> {
  let ts = try!(TransportService::init(cfg));
  Ok(ts.our_hello)
}

#[repr(C, packed)]
struct StartMessage {
    header: MessageHeader,
    options: u32,
    myself: ll::Struct_GNUNET_PeerIdentity,
}

impl StartMessage {
    fn new(options: u32, peer: ll::Struct_GNUNET_PeerIdentity) -> StartMessage {
        let len = ::std::mem::size_of::<StartMessage>();
        StartMessage {
            header: MessageHeader {
                len: (len as u16).to_be(),
                tpe: ll::GNUNET_MESSAGE_TYPE_TRANSPORT_START.to_be(),
            },
            options: options.to_be(),
            myself: peer,
        }
    }
}

impl MessageTrait for StartMessage {
    fn into_slice(&self) -> &[u8] {
        message_to_slice!(StartMessage, self)
    }
}
