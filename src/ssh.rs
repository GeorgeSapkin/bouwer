// Copyright (C) 2026 George Sapkin
//
// SPDX-License-Identifier: GPL-3.0-only

use anyhow::Context;
use ssh2::{Channel, Session};
use std::net::{TcpStream, ToSocketAddrs};
use std::ops::{Deref, DerefMut};
use std::time::Duration;

const SSH_TIMEOUT: u32 = 10_000;

pub struct ChannelGuard {
    channel: Channel,
}

impl ChannelGuard {
    fn new(channel: Channel) -> Self {
        Self { channel }
    }
}

impl Deref for ChannelGuard {
    type Target = Channel;

    fn deref(&self) -> &Self::Target {
        &self.channel
    }
}

impl DerefMut for ChannelGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.channel
    }
}

impl Drop for ChannelGuard {
    fn drop(&mut self) {
        if let Err(e) = self.channel.wait_close() {
            eprintln!("Error waiting for SSH channel to close: {e}");
        }
    }
}

pub struct SshOptions<'options> {
    pub host: &'options str,
    pub user: &'options str,
    pub identity: Option<&'options str>,
    pub password: Option<&'options str>,
}

pub struct Ssh<'ssh> {
    options: SshOptions<'ssh>,
}

impl<'ssh> Ssh<'ssh> {
    pub fn new(options: SshOptions<'ssh>) -> Self {
        Self { options }
    }

    pub fn connect(&self) -> anyhow::Result<ChannelGuard> {
        let timeout = Duration::from_millis(SSH_TIMEOUT.into());
        let addr = (self.options.host, 22)
            .to_socket_addrs()?
            .next()
            .context("Failed to resolve host address")?;

        let tcp = TcpStream::connect_timeout(&addr, timeout)?;
        tcp.set_read_timeout(Some(timeout))?;
        tcp.set_write_timeout(Some(timeout))?;

        let mut session = Session::new()?;
        session.set_timeout(SSH_TIMEOUT);
        session.set_tcp_stream(tcp);
        session.handshake()?;

        let _ = session.auth_methods(self.options.user);
        if !session.authenticated() {
            match (self.options.password, self.options.identity) {
                (Some(password), _) => session.userauth_password(self.options.user, password)?,
                (None, Some(identity_comment)) => {
                    let mut agent = session.agent()?;
                    agent.connect()?;
                    agent.list_identities()?;
                    let identities = agent.identities()?;
                    let identity = identities
                        .iter()
                        .find(|i| i.comment() == identity_comment)
                        .context("Identity not found in SSH agent")?;
                    agent.userauth(self.options.user, identity)?;
                }
                (None, None) => session.userauth_agent(self.options.user)?,
            }
        }

        anyhow::ensure!(session.authenticated(), "Authentication failed");

        let channel = session
            .channel_session()
            .context("Failed to open SSH channel")?;
        Ok(ChannelGuard::new(channel))
    }

    pub fn list_identities() -> anyhow::Result<Vec<String>> {
        let session = Session::new()?;
        let mut agent = session.agent().context("Failed to initialize SSH agent")?;
        agent.connect().context("Failed to connect to SSH agent")?;
        agent
            .list_identities()
            .context("Failed to list agent identities")?;

        let mut identities: Vec<String> = agent
            .identities()?
            .into_iter()
            .filter_map(|i| (!i.comment().is_empty()).then(|| i.comment().to_string()))
            .collect();
        identities.sort();

        Ok(identities)
    }
}
