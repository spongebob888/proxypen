use shadowquic::msgs::socks5::{
    AuthReply, AuthReq, CmdReply, CmdReq, PasswordAuthReply, PasswordAuthReq, SocksAddr, VarVec,
    consts::*,
};
use shadowquic::msgs::{SDecode, SEncode};
use tokio::net::TcpStream;

use crate::config::ProxyAuth;
use crate::error::{ProxyPenError, Result};

/// Perform SOCKS5 authentication on an established TCP connection.
pub async fn authenticate(stream: &mut TcpStream, auth: Option<&ProxyAuth>) -> Result<()> {
    let method = if auth.is_some() {
        SOCKS5_AUTH_METHOD_PASSWORD
    } else {
        SOCKS5_AUTH_METHOD_NONE
    };

    let auth_req = AuthReq {
        version: SOCKS5_VERSION,
        methods: VarVec {
            len: 1,
            contents: vec![method],
        },
    };

    auth_req.encode(stream).await?;
    let rep = AuthReply::decode(stream).await?;

    if rep.version != SOCKS5_VERSION {
        return Err(ProxyPenError::Socks("server version mismatch".into()));
    }
    if rep.method != method {
        return Err(ProxyPenError::Socks(format!(
            "auth method not supported (wanted {method:#x}, got {:#x})",
            rep.method
        )));
    }

    if let Some(cred) = auth {
        let pw_req = PasswordAuthReq {
            version: 0x01,
            username: VarVec {
                len: cred.username.len() as u8,
                contents: cred.username.as_bytes().to_vec(),
            },
            password: VarVec {
                len: cred.password.len() as u8,
                contents: cred.password.as_bytes().to_vec(),
            },
        };
        pw_req.encode(stream).await?;
        let pw_rep = PasswordAuthReply::decode(stream).await?;
        if pw_rep.status != SOCKS5_REPLY_SUCCEEDED {
            return Err(ProxyPenError::Socks("authentication failed".into()));
        }
    }

    Ok(())
}

/// Send SOCKS5 TCP CONNECT command and return the reply.
pub async fn send_connect(stream: &mut TcpStream, dst: SocksAddr) -> Result<CmdReply> {
    let req = CmdReq {
        version: SOCKS5_VERSION,
        cmd: SOCKS5_CMD_TCP_CONNECT,
        rsv: SOCKS5_RESERVE,
        dst,
    };
    req.encode(stream).await?;
    let reply = CmdReply::decode(stream).await?;

    if reply.rep != SOCKS5_REPLY_SUCCEEDED {
        return Err(ProxyPenError::Socks(format!(
            "CONNECT failed with reply code {:#x}",
            reply.rep
        )));
    }
    Ok(reply)
}

/// Send SOCKS5 UDP ASSOCIATE command and return the reply.
pub async fn send_udp_associate(stream: &mut TcpStream, bind_hint: SocksAddr) -> Result<CmdReply> {
    let req = CmdReq {
        version: SOCKS5_VERSION,
        cmd: SOCKS5_CMD_UDP_ASSOCIATE,
        rsv: SOCKS5_RESERVE,
        dst: bind_hint,
    };
    req.encode(stream).await?;
    let reply = CmdReply::decode(stream).await?;

    if reply.rep != SOCKS5_REPLY_SUCCEEDED {
        return Err(ProxyPenError::Socks(format!(
            "UDP ASSOCIATE failed with reply code {:#x}",
            reply.rep
        )));
    }
    Ok(reply)
}
