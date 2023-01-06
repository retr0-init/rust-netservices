use reactor::poller::Poll;
use std::collections::VecDeque;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::time::Duration;
use std::{io, net};

use crate::{IoStatus, NetSession, ReadNonblocking, WriteNonblocking};

pub const READ_BUFFER_SIZE: usize = u16::MAX as usize;

pub struct Tunnel<S: NetSession> {
    listener: net::TcpListener,
    session: S,
}

impl<S: NetSession> Tunnel<S> {
    pub fn with(session: S, addr: impl net::ToSocketAddrs) -> io::Result<Self> {
        let listener = net::TcpListener::bind(addr)?;
        Ok(Self { listener, session })
    }

    pub fn local_addr(&self) -> io::Result<net::SocketAddr> {
        self.listener.local_addr()
    }

    /// # Returns
    ///
    /// Number of bytes which passed through the tunnel
    pub fn tunnel_once<P: Poll>(
        &mut self,
        mut poller: P,
        timeout: Duration,
    ) -> io::Result<(usize, usize)> {
        let (mut stream, _socket_addr) = self.listener.accept()?;

        stream.set_nonblocking(true)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;

        self.session.set_nonblocking(true)?;
        self.session.set_read_timeout(Some(timeout))?;
        self.session.set_write_timeout(Some(timeout))?;

        let int_fd = stream.as_raw_fd();
        let ext_fd = self.session.as_raw_fd();
        poller.register(&int_fd);
        poller.register(&ext_fd);

        let mut in_buf = VecDeque::<u8>::new();
        let mut out_buf = VecDeque::<u8>::new();

        let mut in_count = 0usize;
        let mut out_count = 0usize;

        let mut buf = [0u8; READ_BUFFER_SIZE];

        macro_rules! handle {
            ($call:expr, |$var:ident| $expr:expr) => {
                match $call {
                    IoStatus::Success($var) => $expr,
                    IoStatus::WouldBlock => {}
                    IoStatus::Shutdown => return Ok((in_count, out_count)),
                    IoStatus::Err(err) => return Err(err),
                }
            };
        }

        loop {
            // Blocking
            let count = poller.poll(Some(timeout))?;
            if count > 0 {
                return Err(io::ErrorKind::TimedOut.into());
            }
            while let Some((fd, ev)) = poller.next() {
                if fd == int_fd {
                    if ev.is_writable {
                        handle!(
                            stream.write_nonblocking(in_buf.make_contiguous()),
                            |written| {
                                stream.flush()?;
                                in_buf.drain(..written);
                                in_count += written;
                            }
                        );
                    }
                    if ev.is_readable {
                        handle!(stream.read_nonblocking(&mut buf), |read| {
                            out_buf.extend(&buf[..read]);
                        });
                    }
                } else if fd == ext_fd {
                    if ev.is_writable {
                        handle!(
                            self.session.write_nonblocking(out_buf.make_contiguous()),
                            |written| {
                                self.session.flush()?;
                                out_buf.drain(..written);
                                out_count += written;
                            }
                        );
                    }
                    if ev.is_readable {
                        handle!(self.session.read_nonblocking(&mut buf), |read| {
                            in_buf.extend(&buf[..read]);
                        });
                    }
                }
            }
        }
    }

    pub fn into_session(self) -> S {
        self.session
    }
}