use super::error::{ErrorKind, Result, ResultExt};
use super::header::{decode, encode};
use super::{ServicePair, ServiceResult};
use crate::rosmsg::RosMsg;
use byteorder::{LittleEndian, ReadBytesExt};
use std;
use std::collections::HashMap;
use std::io;
use std::io::Write;
use std::net::TcpStream;
use std::sync::Arc;
use std::thread;

pub struct ClientResponse<T> {
    handle: thread::JoinHandle<Result<ServiceResult<T>>>,
}

impl<T> ClientResponse<T> {
    pub fn read(self) -> Result<ServiceResult<T>> {
        self.handle
            .join()
            .unwrap_or_else(|_| Err(ErrorKind::ServiceResponseUnknown.into()))
    }
}

impl<T: Send + 'static> ClientResponse<T> {
    pub fn callback<F>(self, callback: F)
    where
        F: FnOnce(Result<ServiceResult<T>>) + Send + 'static,
    {
        thread::spawn(move || callback(self.read()));
    }
}

struct ClientInfo {
    caller_id: String,
    uri: String,
    service: String,
}

#[derive(Clone)]
pub struct Client<T: ServicePair> {
    info: std::sync::Arc<ClientInfo>,
    phantom: std::marker::PhantomData<T>,
}

impl<T: ServicePair> Client<T> {
    pub fn new(caller_id: &str, uri: &str, service: &str) -> Client<T> {
        Client {
            info: std::sync::Arc::new(ClientInfo {
                caller_id: String::from(caller_id),
                uri: String::from(uri),
                service: String::from(service),
            }),
            phantom: std::marker::PhantomData,
        }
    }

    pub fn req(&self, args: &T::Request) -> Result<ServiceResult<T::Response>> {
        Self::request_body(
            args,
            &self.info.uri,
            &self.info.caller_id,
            &self.info.service,
        )
    }

    pub fn req_async(&self, args: T::Request) -> ClientResponse<T::Response> {
        let info = Arc::clone(&self.info);
        ClientResponse {
            handle: thread::spawn(move || {
                Self::request_body(&args, &info.uri, &info.caller_id, &info.service)
            }),
        }
    }

    fn request_body(
        args: &T::Request,
        uri: &str,
        caller_id: &str,
        service: &str,
    ) -> Result<ServiceResult<T::Response>> {
        let connection = TcpStream::connect(uri.trim_start_matches("rosrpc://"));
        let mut stream = connection
            .chain_err(|| ErrorKind::ServiceConnectionFail(service.into(), uri.into()))?;

        // Service request starts by exchanging connection headers
        exchange_headers::<T, _>(&mut stream, caller_id, service)?;

        let mut writer = io::Cursor::new(Vec::with_capacity(128));
        // skip the first 4 bytes that will contain the message length
        writer.set_position(4);

        args.encode(&mut writer)?;

        // write the message length to the start of the header
        let message_length = (writer.position() - 4) as u32;
        writer.set_position(0);
        message_length.encode(&mut writer)?;

        // Send request to service
        stream.write_all(&writer.into_inner())?;

        // Service responds with a boolean byte, signalling success
        let success = read_verification_byte(&mut stream)
            .chain_err(|| ErrorKind::ServiceResponseInterruption)?;
        Ok(if success {
            // Decode response as response type upon success

            // TODO: validate response length
            let _length = stream.read_u32::<LittleEndian>();

            Ok(RosMsg::decode(&mut stream)?)
        } else {
            // Decode response as string upon failure
            Err(RosMsg::decode(&mut stream)?)
        })
    }
}

#[inline]
fn read_verification_byte<R: std::io::Read>(reader: &mut R) -> std::io::Result<bool> {
    reader.read_u8().map(|v| v != 0)
}

fn write_request<T, U>(mut stream: &mut U, caller_id: &str, service: &str) -> Result<()>
where
    T: ServicePair,
    U: std::io::Write,
{
    let mut fields = HashMap::<String, String>::new();
    fields.insert(String::from("callerid"), String::from(caller_id));
    fields.insert(String::from("service"), String::from(service));
    fields.insert(String::from("md5sum"), T::md5sum());
    fields.insert(String::from("type"), T::msg_type());
    encode(&mut stream, &fields)?;
    Ok(())
}

fn read_response<T, U>(mut stream: &mut U) -> Result<()>
where
    T: ServicePair,
    U: std::io::Read,
{
    let fields = decode(&mut stream)?;
    if fields.get("callerid").is_none() {
        bail!(ErrorKind::HeaderMissingField("callerid".into()));
    }
    Ok(())
}

fn exchange_headers<T, U>(stream: &mut U, caller_id: &str, service: &str) -> Result<()>
where
    T: ServicePair,
    U: std::io::Write + std::io::Read,
{
    write_request::<T, U>(stream, caller_id, service)?;
    read_response::<T, U>(stream)
}
