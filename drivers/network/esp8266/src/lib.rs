#![no_std]
#![allow(incomplete_features)]
#![feature(min_type_alias_impl_trait)]
#![feature(generic_associated_types)]

mod buffer;
mod num;
mod parser;
mod protocol;
mod socket_pool;

use socket_pool::SocketPool;

use core::future::Future;
use drogue_device_kernel::channel::*;
use drogue_network::{
    ip::{IpAddress, IpProtocol, SocketAddress},
    tcp::{TcpError, TcpStack},
    wifi::{Join, JoinError, WifiSupplicant},
};
use embassy::traits::uart::{Read, Write};
use futures::future::{select, Either};
//use crate::driver::queue::spsc_queue::*;
use buffer::Buffer;
use embedded_hal::digital::v2::OutputPin;
use protocol::{Command, ConnectionType, Response as AtResponse, WiFiMode};

pub const BUFFER_LEN: usize = 512;

#[derive(Debug)]
pub enum AdapterError {
    UnableToInitialize,
    NoAvailableSockets,
    Timeout,
    UnableToOpen,
    UnableToClose,
    WriteError,
    ReadError,
    InvalidSocket,
    OperationNotSupported,
}

pub struct Esp8266WifiController<'a, UART, ENABLE, RESET>
where
    UART: Read + Write + 'static,
    ENABLE: OutputPin + 'static,
    RESET: OutputPin + 'static,
{
    socket_pool: SocketPool,
    command_producer: ChannelSender<'a, Command, consts::U2>,
    response_consumer: ChannelReceiver<'a, AtResponse, consts::U2>,
    notification_consumer: ChannelReceiver<'a, AtResponse, consts::U2>,
}

pub struct Esp8266Modem<'a, UART, ENABLE, RESET>
where
    UART: Read + Write + 'static,
    ENABLE: OutputPin + 'static,
    RESET: OutputPin + 'static,
{
    uart: UART,
    parse_buffer: Buffer,
    command_consumer: ChannelReceiver<'a, Command, consts::U2>,
    response_producer: ChannelSender<'a, AtResponse, consts::U2>,
    notification_producer: ChannelSender<'a, AtResponse, consts::U2>,
}

pub struct Esp8266Wifi<'a, UART, ENABLE, RESET>
where
    UART: Read + Write + 'static,
    ENABLE: OutputPin + 'static,
    RESET: OutputPin + 'static,
{
    enable: ENABLE,
    reset: RESET,
    command_channel: Channel<Command, consts::U2>,
    response_channel: Channel<AtResponse, consts::U2>,
    notification_channel: Channel<AtResponse, consts::U2>,
}

impl<'a, UART, ENABLE, RESET> Esp8266Wifi<'a, UART, ENABLE, RESET>
where
    UART: Read + Write + 'static,
    ENABLE: OutputPin + 'static,
    RESET: OutputPin + 'static,
{
    pub fn new(uart: UART, enable: ENABLE, reset: RESET) -> Self {
        Self {
            uart,
            socket_pool: SocketPool::new(),
            parse_buffer: Buffer::new(),
            response_queue: Channel::new(),
            notification_queue: Channel::new(),
            enable,
            reset,
        }
    }

    async fn digest(&'a mut self) -> Result<(), AdapterError> {
        let result = self.parse_buffer.parse();

        if let Ok(response) = result {
            if !matches!(response, AtResponse::None) {
                log::trace!("--> {:?}", response);
            }
            match response {
                AtResponse::None => {}
                AtResponse::Ok
                | AtResponse::Error
                | AtResponse::FirmwareInfo(..)
                | AtResponse::Connect(..)
                | AtResponse::ReadyForData
                | AtResponse::ReceivedDataToSend(..)
                | AtResponse::DataReceived(..)
                | AtResponse::SendOk
                | AtResponse::SendFail
                | AtResponse::WifiConnectionFailure(..)
                | AtResponse::IpAddress(..)
                | AtResponse::Resolvers(..)
                | AtResponse::DnsFail
                | AtResponse::UnlinkFail
                | AtResponse::IpAddresses(..) => {
                    self.response_producer.send(response).await;
                }
                AtResponse::Closed(..) | AtResponse::DataAvailable { .. } => {
                    self.notification_producer.send(response).await;
                }
                AtResponse::WifiConnected => {
                    log::info!("wifi connected");
                }
                AtResponse::WifiDisconnect => {
                    log::info!("wifi disconnect");
                }
                AtResponse::GotIp => {
                    log::info!("wifi got ip");
                }
            }
        }
        Ok(())
    }

    // Await input from uart and attempt to digest input
    pub async fn run(&'a mut self) -> Result<(), AdapterError> {
        loop {
            let mut buf = [0; 1];
            let command_fut = self.command_consumer.receive();
            let uart_fut = self.uart.read(&mut buf[..]);

            match select(command_fut, uart_fut).await {
                Either::Left((r, _)) => {
                    // Write command to uart
                }
                Either::Right(_) => {
                    self.parse_buffer.write(buf[0]).unwrap();
                    self.digest().await
                }
            }
        }
    }

    /*
    async fn start(mut self) -> Self {
        log::info!("Starting ESP8266 Modem");
        loop {
            if let Err(e) = self.process().await {
                log::error!("Error reading data: {:?}", e);
            }

            if let Err(e) = self.digest().await {
                log::error!("Error digesting data");
            }
        }
    }
    */

    async fn initialize(&mut self) {
        let mut buffer: [u8; 1024] = [0; 1024];
        let mut pos = 0;

        const READY: [u8; 7] = *b"ready\r\n";

        self.enable.set_high().ok().unwrap();
        self.reset.set_high().ok().unwrap();

        log::info!("waiting for adapter to become ready");

        let mut rx_buf = [0; 1];
        loop {
            let result = self.uart.read(&mut rx_buf[..]).await;
            match result {
                Ok(c) => {
                    buffer[pos] = rx_buf[0];
                    pos += 1;
                    if pos >= READY.len() && buffer[pos - READY.len()..pos] == READY {
                        log::info!("adapter is ready");
                        self.disable_echo()
                            .await
                            .expect("Error disabling echo mode");
                        log::info!("Echo disabled");
                        self.enable_mux().await.expect("Error enabling mux");
                        log::info!("Mux enabled");
                        self.set_recv_mode()
                            .await
                            .expect("Error setting receive mode");
                        log::info!("Recv mode configured");
                        self.set_mode().await.expect("Error setting station mode");
                        log::info!("adapter configured");
                        break;
                    }
                }
                Err(e) => {
                    log::error!("Error initializing ESP8266 modem");
                    break;
                }
            }
        }
    }

    async fn set_mode(&mut self) -> Result<(), AdapterError> {
        self.uart
            .write(b"AT+CWMODE_CUR=1\r\n")
            .await
            .map_err(|_| AdapterError::UnableToInitialize)?;
        Ok(self
            .wait_for_ok()
            .await
            .map_err(|_| AdapterError::UnableToInitialize)?)
    }

    async fn disable_echo(&mut self) -> Result<(), AdapterError> {
        self.uart
            .write(b"ATE0\r\n")
            .await
            .map_err(|_| AdapterError::UnableToInitialize)?;
        Ok(self
            .wait_for_ok()
            .await
            .map_err(|_| AdapterError::UnableToInitialize)?)
    }

    async fn enable_mux(&mut self) -> Result<(), AdapterError> {
        self.uart
            .write(b"AT+CIPMUX=1\r\n")
            .await
            .map_err(|_| AdapterError::UnableToInitialize)?;
        Ok(self
            .wait_for_ok()
            .await
            .map_err(|_| AdapterError::UnableToInitialize)?)
    }

    async fn set_recv_mode(&mut self) -> Result<(), AdapterError> {
        self.uart
            .write(b"AT+CIPRECVMODE=1\r\n")
            .await
            .map_err(|_| AdapterError::UnableToInitialize)?;
        Ok(self
            .wait_for_ok()
            .await
            .map_err(|_| AdapterError::UnableToInitialize)?)
    }

    async fn wait_for_ok(&mut self) -> Result<(), AdapterError> {
        let mut buf: [u8; 64] = [0; 64];
        let mut pos = 0;

        loop {
            self.uart
                .read(&mut buf[pos..pos + 1])
                .await
                .map_err(|_| AdapterError::ReadError)?;
            pos += 1;
            if buf[0..pos].ends_with(b"OK\r\n") {
                return Ok(());
            } else if buf[0..pos].ends_with(b"ERROR\r\n") {
                return Err(AdapterError::UnableToInitialize);
            }
        }
    }

    async fn send<'c>(&mut self, command: Command<'c>) -> Result<AtResponse, AdapterError> {
        let bytes = command.as_bytes();
        log::trace!(
            "writing command {}",
            core::str::from_utf8(bytes.as_bytes()).unwrap()
        );

        self.uart
            .write(&bytes.as_bytes())
            .await
            .map_err(|e| AdapterError::WriteError)?;

        self.uart
            .write(b"\r\n")
            .await
            .map_err(|e| AdapterError::WriteError)?;

        Ok(self.wait_for_response().await)
    }

    async fn wait_for_response(&mut self) -> AtResponse {
        self.response_queue.receive().await
    }

    async fn set_wifi_mode(&mut self, mode: WiFiMode) -> Result<(), ()> {
        let command = Command::SetMode(mode);
        match self.send(command).await {
            Ok(AtResponse::Ok) => Ok(()),
            _ => Err(()),
        }
    }

    async fn join_wep(&mut self, ssid: &str, password: &str) -> Result<IpAddress, JoinError> {
        let command = Command::JoinAp { ssid, password };
        match self.send(command).await {
            Ok(AtResponse::Ok) => self.get_ip_address().await.map_err(|_| JoinError::Unknown),
            Ok(AtResponse::WifiConnectionFailure(reason)) => {
                log::warn!("Error connecting to wifi: {:?}", reason);
                Err(JoinError::Unknown)
            }
            _ => Err(JoinError::UnableToAssociate),
        }
    }

    async fn get_ip_address(&mut self) -> Result<IpAddress, ()> {
        let command = Command::QueryIpAddress;

        if let Ok(AtResponse::IpAddresses(addresses)) = self.send(command).await {
            return Ok(IpAddress::V4(addresses.ip));
        }

        Err(())
    }

    async fn process_notifications(&mut self) {
        while let Some(response) = self.notification_queue.try_receive().await {
            match response {
                AtResponse::DataAvailable { link_id, len } => {
                    //  shared.socket_pool // [link_id].available += len;
                }
                AtResponse::Connect(_) => {}
                AtResponse::Closed(link_id) => {
                    self.socket_pool.close(link_id as u8);
                }
                _ => { /* ignore */ }
            }
        }
    }
}

impl<'a, UART, ENABLE, RESET> WifiSupplicant for Esp8266Wifi<'a, UART, ENABLE, RESET>
where
    UART: Read + Write + 'static,
    ENABLE: OutputPin + 'static,
    RESET: OutputPin + 'static,
{
    type JoinFuture<'m> = impl Future<Output = Result<IpAddress, JoinError>> + 'm;
    fn join<'m>(&'m mut self, join_info: Join) -> Self::JoinFuture<'m> {
        async move {
            match join_info {
                Join::Open => Err(JoinError::Unknown),
                Join::Wpa { ssid, password } => {
                    self.join_wep(ssid.as_ref(), password.as_ref()).await
                }
            }
        }
    }
}

impl<'a, UART, ENABLE, RESET> TcpStack for Esp8266Wifi<'a, UART, ENABLE, RESET>
where
    UART: Read + Write + 'static,
    ENABLE: OutputPin + 'static,
    RESET: OutputPin + 'static,
{
    type SocketHandle = u8;

    #[rustfmt::skip]
    type OpenFuture<'m> where 'a: 'm = impl Future<Output = Self::SocketHandle> + 'm;
    fn open<'m>(&'m self) -> Self::OpenFuture<'m> {
        async move { self.socket_pool.open().await }
    }

    #[rustfmt::skip]
    type ConnectFuture<'m> where 'a: 'm = impl Future<Output = Result<(), TcpError>> + 'm;
    fn connect<'m>(
        &'m mut self,
        handle: Self::SocketHandle,
        proto: IpProtocol,
        dst: SocketAddress,
    ) -> Self::ConnectFuture<'m> {
        async move {
            let command = Command::StartConnection(handle as usize, ConnectionType::TCP, dst);
            if let Ok(AtResponse::Connect(..)) = self.send(command).await {
                Ok(())
            } else {
                Err(TcpError::ConnectError)
            }
        }
    }

    #[rustfmt::skip]
    type WriteFuture<'m> where 'a: 'm = impl Future<Output = Result<usize, TcpError>> + 'm;
    fn write<'m>(&'m mut self, handle: Self::SocketHandle, buf: &'m [u8]) -> Self::WriteFuture<'m> {
        async move {
            self.process_notifications().await;
            if self.socket_pool.is_closed(handle) {
                return Err(TcpError::SocketClosed);
            }
            let command = Command::Send {
                link_id: handle as usize,
                len: buf.len(),
            };

            let result = match self.send(command).await {
                Ok(AtResponse::Ok) => {
                    match self.wait_for_response().await {
                        AtResponse::ReadyForData => {
                            if let Ok(_) = self.uart.write(buf).await {
                                let mut data_sent: Option<usize> = None;
                                loop {
                                    match self.wait_for_response().await {
                                        AtResponse::ReceivedDataToSend(len) => {
                                            data_sent.replace(len);
                                        }
                                        AtResponse::SendOk => {
                                            break Ok(data_sent.unwrap_or_default())
                                        }
                                        _ => {
                                            break Err(TcpError::WriteError);
                                            // unknown response
                                        }
                                    }
                                }
                            } else {
                                Err(TcpError::WriteError)
                            }
                        }
                        r => {
                            log::info!("Unexpected response: {:?}", r);
                            Err(TcpError::WriteError)
                        }
                    }
                }
                Ok(r) => {
                    log::info!("Unexpected response: {:?}", r);
                    Err(TcpError::WriteError)
                }
                Err(_) => Err(TcpError::WriteError),
            };
            result
        }
    }

    #[rustfmt::skip]
    type ReadFuture<'m> where 'a: 'm = impl Future<Output = Result<usize, TcpError>> + 'm;
    fn read<'m>(
        &'m mut self,
        handle: Self::SocketHandle,
        buf: &'m mut [u8],
    ) -> Self::ReadFuture<'m> {
        async move {
            let mut rp = 0;
            loop {
                let result = async {
                    self.process_notifications().await;
                    if self.shared.as_ref().unwrap().socket_pool.is_closed(handle) {
                        return Err(TcpError::SocketClosed);
                    }

                    let command = Command::Receive {
                        link_id: handle as usize,
                        len: core::cmp::min(buf.len() - rp, BUFFER_LEN),
                    };

                    match self.send(command).await {
                        Ok(AtResponse::DataReceived(inbound, len)) => {
                            for (i, b) in inbound[0..len].iter().enumerate() {
                                buf[rp + i] = *b;
                            }
                            Ok(len)
                        }
                        Ok(AtResponse::Ok) => Ok(0),
                        _ => Err(TcpError::ReadError),
                    }
                }
                .await;

                match result {
                    Ok(len) => {
                        rp += len;
                        if len == 0 || rp == buf.len() {
                            return Ok(rp);
                        }
                    }
                    Err(e) => {
                        if rp == 0 {
                            return Err(e);
                        } else {
                            return Ok(rp);
                        }
                    }
                }
            }
        }
    }

    #[rustfmt::skip]
    type CloseFuture<'m> where 'a: 'm = impl Future<Output = ()> + 'm;
    fn close<'m>(&'m mut self, handle: Self::SocketHandle) -> Self::CloseFuture<'m> {
        async move {
            let command = Command::CloseConnection(handle as usize);
            match self.send(command).await {
                Ok(AtResponse::Ok) | Ok(AtResponse::UnlinkFail) => {
                    self.socket_pool.close(handle);
                }
                _ => {}
            }
        }
    }
}
