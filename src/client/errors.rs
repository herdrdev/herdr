use std::io;

use crate::protocol;
use crate::server::socket_paths::client_socket_path;

/// Errors that can occur during client operation.
#[derive(Debug)]
pub enum ClientError {
    /// Could not connect to the server's client socket.
    ConnectionFailed(io::Error),
    /// Server rejected our handshake.
    HandshakeRejected { version: u32, error: String },
    /// Server shut down.
    ServerShutdown { reason: Option<String> },
    /// Lost connection to the server.
    ConnectionLost(io::Error),
    /// Protocol error (framing, deserialization).
    Protocol(protocol::FramingError),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::ConnectionFailed(err) => {
                write!(f, "não foi possível conectar ao servidor: {err}")?;
                let path = client_socket_path();
                write!(f, "\nAbra novamente o Pretorian para iniciar o servidor.")?;
                write!(f, "\nCaminho do socket: {}", path.display())
            }
            ClientError::HandshakeRejected { version, error } => {
                write!(
                    f,
                    "o servidor recusou a conexão inicial (versão {version}): {error}"
                )
            }
            ClientError::ServerShutdown { reason } => {
                match reason.as_deref() {
                    Some("detached") => {
                        if let Ok(reattach_command) =
                            std::env::var(crate::remote::REATTACH_COMMAND_ENV_VAR)
                        {
                            write!(f, "desconectado do servidor remoto")?;
                            write!(f, "\nExecute `{reattach_command}` para reconectar")?;
                        } else {
                            write!(f, "desconectado do servidor")?;
                            write!(f, "\nAbra novamente o Pretorian para reconectar")?;
                        }
                    }
                    _ => {
                        write!(f, "o servidor foi encerrado")?;
                        if let Some(reason) = reason {
                            write!(f, ": {reason}")?;
                        }
                    }
                }
                Ok(())
            }
            ClientError::ConnectionLost(err) => {
                if let Ok(reattach_command) = std::env::var(crate::remote::REATTACH_COMMAND_ENV_VAR)
                {
                    write!(f, "a conexão com o Herdr remoto foi perdida: {err}")?;
                    write!(f, "\nSe o servidor remoto permaneceu ativo após a queda de SSH ou da rede, seus painéis podem continuar em execução.")?;
                    write!(f, "\nExecute `{reattach_command}` para reconectar")
                } else {
                    write!(f, "a conexão com o servidor foi perdida: {err}")
                }
            }
            ClientError::Protocol(err) => write!(f, "erro de protocolo: {err}"),
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ClientError::ConnectionFailed(err) => Some(err),
            ClientError::ConnectionLost(err) => Some(err),
            ClientError::Protocol(err) => Some(err),
            _ => None,
        }
    }
}

impl From<protocol::FramingError> for ClientError {
    fn from(err: protocol::FramingError) -> Self {
        match err {
            protocol::FramingError::UnexpectedEof => ClientError::ConnectionLost(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "o servidor encerrou a conexão",
            )),
            protocol::FramingError::Io(err) => ClientError::ConnectionLost(err),
            err => ClientError::Protocol(err),
        }
    }
}
