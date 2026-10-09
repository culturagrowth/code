//! Error type shared by the whole crate. User-facing messages are in Portuguese and never
//! contain secrets, signatures or presigned URLs.

use duoclip_presence::HeartbeatFailure;

/// Everything that can go wrong talking to the Worker or handling the identity file.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NetError {
    /// The Worker answered with an error status. `code` is the Worker's stable machine-readable
    /// error code (`{"error": "..."}`) when the body had one.
    #[error("{}", http_message(*status, code.as_deref()))]
    Http {
        /// HTTP status code.
        status: u16,
        /// Worker error code such as `not_a_member`, when present and well formed.
        code: Option<String>,
    },
    /// No usable response (connection, TLS, timeout). The text never includes URLs.
    #[error("falha de rede: {0}")]
    Network(String),
    /// The Worker answered but the body was not what the protocol promises.
    #[error("resposta inesperada do servidor: {0}")]
    InvalidResponse(String),
    /// Bad local input or configuration (URL, names, identity file).
    #[error("{0}")]
    Config(String),
}

impl NetError {
    /// The Worker error code of an [`NetError::Http`], if any.
    pub fn code(&self) -> Option<&str> {
        match self {
            NetError::Http { code, .. } => code.as_deref(),
            _ => None,
        }
    }

    /// HTTP status of an [`NetError::Http`], if any.
    pub fn status(&self) -> Option<u16> {
        match self {
            NetError::Http { status, .. } => Some(*status),
            _ => None,
        }
    }
}

/// Maps a failed heartbeat call onto the presence adapter's failure kinds. A `409` (the Worker
/// holds a newer or equal run/sequence pair) becomes [`HeartbeatFailure::Stale409`].
impl From<&NetError> for HeartbeatFailure {
    fn from(e: &NetError) -> Self {
        match e {
            NetError::Http { status: 409, .. } => HeartbeatFailure::Stale409,
            NetError::Http { status, .. } => HeartbeatFailure::Http(*status),
            NetError::InvalidResponse(_) => HeartbeatFailure::InvalidResponse,
            NetError::Network(_) | NetError::Config(_) => HeartbeatFailure::Network,
        }
    }
}

fn http_message(status: u16, code: Option<&str>) -> String {
    let known = match code {
        Some("unauthorized") => Some(
            "o servidor recusou a assinatura deste PC (confira o relógio do Windows e se o PC está registrado com `configurar`)",
        ),
        Some("not_a_member") => Some("você não é membro deste grupo"),
        Some("invalid_invite") => Some("código de convite inválido, vencido ou sem usos"),
        Some("too_many_attempts") => {
            Some("muitas tentativas de entrar com código hoje; tente de novo amanhã")
        }
        Some("crew_limit") => Some("limite de grupos criados por este PC atingido"),
        Some("invite_limit") => {
            Some("este grupo já tem convites válidos demais; espere algum vencer ou ser usado")
        }
        Some("registration_limited") => Some(
            "o servidor não aceita novos cadastros hoje; tente amanhã ou fale com o dono do servidor",
        ),
        Some("device_exists") => {
            Some("este identificador de PC já está registrado com outra chave")
        }
        Some("stale_presence") => Some("o servidor já tem um anúncio de presença mais novo"),
        Some("clip_not_found") => Some("clipe desconhecido"),
        Some("clip_gone") | Some("clip_expired") => Some("o clipe foi apagado ou já expirou"),
        Some("clip_exists") => Some("este identificador de clipe já está em uso"),
        Some("clip_quota_exceeded") | Some("daily_quota_exceeded") | Some("global_quota_exceeded") => {
            Some("cota de envio do servidor atingida")
        }
        Some("pov_mismatch") => Some("um PC só pode enviar o próprio POV"),
        _ => None,
    };
    match (known, code) {
        (Some(text), _) => format!("{text} (HTTP {status})"),
        (None, Some(code)) => format!("o servidor respondeu com erro HTTP {status} ({code})"),
        (None, None) => format!("o servidor respondeu com erro HTTP {status}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_409_maps_to_presence_failure() {
        let e = NetError::Http {
            status: 409,
            code: Some("stale_presence".into()),
        };
        assert_eq!(HeartbeatFailure::from(&e), HeartbeatFailure::Stale409);
        let e = NetError::Http {
            status: 503,
            code: None,
        };
        assert_eq!(HeartbeatFailure::from(&e), HeartbeatFailure::Http(503));
        assert_eq!(
            HeartbeatFailure::from(&NetError::Network("x".into())),
            HeartbeatFailure::Network
        );
        assert_eq!(
            HeartbeatFailure::from(&NetError::InvalidResponse("x".into())),
            HeartbeatFailure::InvalidResponse
        );
    }

    #[test]
    fn messages_are_portuguese_and_carry_status() {
        let e = NetError::Http {
            status: 404,
            code: Some("invalid_invite".into()),
        };
        assert!(e.to_string().contains("convite inválido"));
        assert!(e.to_string().contains("404"));
        let e = NetError::Http {
            status: 500,
            code: None,
        };
        assert!(e.to_string().contains("500"));
    }
}
