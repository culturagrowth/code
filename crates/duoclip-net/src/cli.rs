//! The `duoclip-amigos` console tool (Portuguese): register this PC, create a group, invite
//! friends and join their groups. `src/bin/duoclip-amigos.rs` only calls [`run`].

use std::io::Write;
use std::path::PathBuf;

use duoclip_presence::{parse_canonical_uuid, HeartbeatRequest};
use duoclip_proto::CrewId;

use crate::client::system_now_ms;
use crate::identity::{validate_crew_name, validate_display_name};
use crate::{normalize_base_url, Identity, NetError, WorkerClient};

/// What the user asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `configurar --url <worker> --nome <nome>`
    Configurar {
        /// Worker base URL (optional when the identity file already has one).
        url: Option<String>,
        /// Display name (optional when the identity file already exists).
        nome: Option<String>,
    },
    /// `criar-grupo <nome>`
    CriarGrupo {
        /// Name of the new group.
        nome: String,
    },
    /// `convidar [<grupo>]`
    Convidar {
        /// Group id or name; optional when there is only one group.
        grupo: Option<String>,
    },
    /// `entrar <codigo>`
    Entrar {
        /// The friend's invite code.
        codigo: String,
    },
    /// `grupos`
    Grupos,
    /// `status`
    Status,
    /// `ajuda` / `--help`
    Ajuda,
}

/// A parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// The command.
    pub command: Command,
    /// `--identity <arquivo>` override of the identity file location.
    pub identity_path: Option<PathBuf>,
}

/// The help text.
pub const USAGE: &str = "\
duoclip-amigos: prepara este PC para trocar clipes com os amigos

Uso:
  duoclip-amigos configurar --url https://<worker> --nome \"Nico\"
        Cria a identidade deste PC (mantém a chave se já existir) e registra no servidor.
  duoclip-amigos criar-grupo \"Amigos do LoL\"
        Cria um grupo e mostra o id dele.
  duoclip-amigos convidar [<grupo>]
        Mostra um código de convite (vale 24 horas e 5 usos) para os amigos entrarem.
  duoclip-amigos entrar <codigo>
        Entra no grupo de um amigo com o código dele.
  duoclip-amigos grupos
        Lista seus grupos e quem está neles.
  duoclip-amigos status
        Mostra a identidade deste PC (sem segredos) e se o servidor responde.

Opção geral:
  --identity <arquivo>   usa outro arquivo de identidade (padrão: %APPDATA%\\DuoClip\\identidade.json)
";

/// Parses the arguments after the program name. The error is a Portuguese message.
pub fn parse_args(args: &[String]) -> Result<Invocation, String> {
    let mut url = None;
    let mut nome = None;
    let mut identity_path = None;
    let mut positional: Vec<String> = Vec::new();
    let mut help = false;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_owned())),
            _ => (arg.as_str(), None),
        };
        match flag {
            "-h" | "--help" => help = true,
            "--url" | "--nome" | "--identity" => {
                let value = match inline {
                    Some(v) => v,
                    None => iter
                        .next()
                        .cloned()
                        .ok_or_else(|| format!("falta o valor depois de {flag}"))?,
                };
                match flag {
                    "--url" => url = Some(value),
                    "--nome" => nome = Some(value),
                    _ => identity_path = Some(PathBuf::from(value)),
                }
            }
            f if f.starts_with("--") => return Err(format!("opção desconhecida: {f}")),
            _ => positional.push(arg.clone()),
        }
    }
    if help && positional.is_empty() {
        return Ok(Invocation {
            command: Command::Ajuda,
            identity_path,
        });
    }
    let mut positional = positional.into_iter();
    let name = positional
        .next()
        .ok_or_else(|| "faltou o comando; rode `duoclip-amigos ajuda`".to_owned())?;
    let rest: Vec<String> = positional.collect();
    let joined = (!rest.is_empty()).then(|| rest.join(" "));
    let only_options = |what: &str| {
        if url.is_some() || nome.is_some() {
            Err(format!("`{what}` não aceita --url nem --nome"))
        } else {
            Ok(())
        }
    };
    let no_args = |what: &str| {
        if joined.is_some() {
            Err(format!("`{what}` não aceita argumentos"))
        } else {
            Ok(())
        }
    };
    let command = match name.as_str() {
        "configurar" => {
            no_args("configurar")?;
            Command::Configurar {
                url: url.clone(),
                nome: nome.clone(),
            }
        }
        "criar-grupo" => {
            only_options("criar-grupo")?;
            Command::CriarGrupo {
                nome: joined
                    .ok_or("faltou o nome do grupo: duoclip-amigos criar-grupo \"Meu grupo\"")?,
            }
        }
        "convidar" => {
            only_options("convidar")?;
            Command::Convidar { grupo: joined }
        }
        "entrar" => {
            only_options("entrar")?;
            Command::Entrar {
                codigo: joined.ok_or("faltou o código: duoclip-amigos entrar <codigo>")?,
            }
        }
        "grupos" => {
            only_options("grupos")?;
            no_args("grupos")?;
            Command::Grupos
        }
        "status" => {
            only_options("status")?;
            no_args("status")?;
            Command::Status
        }
        "ajuda" | "help" => Command::Ajuda,
        other => return Err(format!("comando desconhecido: {other}")),
    };
    Ok(Invocation {
        command,
        identity_path,
    })
}

/// Runs the tool. Returns the process exit code: 0 success, 1 failure, 2 wrong usage.
pub fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let invocation = match parse_args(args) {
        Ok(i) => i,
        Err(message) => {
            let _ = writeln!(err, "Erro: {message}\n\n{USAGE}");
            return 2;
        }
    };
    if invocation.command == Command::Ajuda {
        let _ = write!(out, "{USAGE}");
        return 0;
    }
    let path = match invocation
        .identity_path
        .clone()
        .map(Ok)
        .unwrap_or_else(Identity::default_path)
    {
        Ok(p) => p,
        Err(e) => {
            let _ = writeln!(err, "Erro: {e}");
            return 1;
        }
    };
    match execute(&invocation.command, &path, out) {
        Ok(code) => code,
        Err(e) => {
            let _ = writeln!(err, "Erro: {e}");
            1
        }
    }
}

/// A configured and registered identity plus a client for its Worker.
fn ready_client(path: &std::path::Path) -> Result<WorkerClient, NetError> {
    let not_ready = || {
        NetError::Config(
            "este PC ainda não foi configurado; rode primeiro: duoclip-amigos configurar --url https://<worker> --nome \"Seu nome\""
                .into(),
        )
    };
    let identity = Identity::load(path)?.ok_or_else(not_ready)?;
    let url = identity.worker_url.clone().ok_or_else(not_ready)?;
    if !identity.registered {
        return Err(not_ready());
    }
    WorkerClient::new(&url, identity)
}

/// Finds a group by canonical id or (case-insensitive) name; with no argument, the only group.
fn resolve_group(identity: &Identity, wanted: Option<&str>) -> Result<CrewId, NetError> {
    let list = || {
        identity
            .grupos
            .iter()
            .map(|g| format!("  {} ({})", g.nome, g.id))
            .collect::<Vec<_>>()
            .join("\n")
    };
    match wanted {
        None => match identity.grupos.as_slice() {
            [only] => Ok(only.id),
            [] => Err(NetError::Config(
                "você ainda não tem grupos; crie um com `criar-grupo` ou entre em um com `entrar`"
                    .into(),
            )),
            _ => Err(NetError::Config(format!(
                "você tem mais de um grupo; diga qual (nome ou id):\n{}",
                list()
            ))),
        },
        Some(text) => {
            let text = text.trim();
            if let Some(uuid) = parse_canonical_uuid(&text.to_ascii_lowercase()) {
                return Ok(CrewId(uuid));
            }
            let lower = text.to_lowercase();
            let mut matches = identity
                .grupos
                .iter()
                .filter(|g| g.nome.to_lowercase() == lower);
            match (matches.next(), matches.next()) {
                (Some(g), None) => Ok(g.id),
                (Some(_), Some(_)) => Err(NetError::Config(format!(
                    "mais de um grupo com esse nome; use o id:\n{}",
                    list()
                ))),
                _ => Err(NetError::Config(format!(
                    "não achei esse grupo; seus grupos:\n{}",
                    if identity.grupos.is_empty() {
                        "  (nenhum)".to_owned()
                    } else {
                        list()
                    }
                ))),
            }
        }
    }
}

fn short_group_name(id: CrewId) -> String {
    let id = id.to_string();
    format!("Grupo {}", &id[..8])
}

fn execute(
    command: &Command,
    path: &std::path::Path,
    out: &mut dyn Write,
) -> Result<i32, NetError> {
    match command {
        Command::Ajuda => {
            let _ = write!(out, "{USAGE}");
            Ok(0)
        }
        Command::Configurar { url, nome } => configurar(path, url.as_deref(), nome.as_deref(), out),
        Command::CriarGrupo { nome } => {
            let nome = validate_crew_name(nome)?;
            let mut client = ready_client(path)?;
            let id = client.create_crew(&nome)?;
            client.identity_mut().remember_group(id, &nome);
            client.identity().save(path)?;
            let _ = writeln!(out, "Grupo \"{nome}\" criado.");
            let _ = writeln!(out, "Id do grupo: {id}");
            let _ = writeln!(out, "Para chamar os amigos: duoclip-amigos convidar");
            Ok(0)
        }
        Command::Convidar { grupo } => {
            let mut client = ready_client(path)?;
            let crew = resolve_group(client.identity(), grupo.as_deref())?;
            let invite = client.create_invite(crew)?;
            let nome = client
                .identity()
                .group_name(crew)
                .map_or_else(|| crew.to_string(), str::to_owned);
            let _ = writeln!(out, "Código de convite para \"{nome}\": {}", invite.code);
            let _ = writeln!(out, "Vale por 24 horas e 5 usos. Seus amigos entram com:");
            let _ = writeln!(out, "  duoclip-amigos entrar {}", invite.code);
            Ok(0)
        }
        Command::Entrar { codigo } => {
            let mut client = ready_client(path)?;
            let crew = client.join_crew(codigo)?;
            if client.identity().group_name(crew).is_none() {
                client
                    .identity_mut()
                    .remember_group(crew, &short_group_name(crew));
                client.identity().save(path)?;
            }
            let nome = client
                .identity()
                .group_name(crew)
                .map_or_else(|| crew.to_string(), str::to_owned);
            let _ = writeln!(out, "Você entrou no grupo \"{nome}\" ({crew}).");
            let members = client.members(crew)?;
            let me = client.identity().device_id();
            let _ = writeln!(out, "Membros:");
            for m in members {
                let you = if m.device_id == me { " (você)" } else { "" };
                let _ = writeln!(out, "  {}{you}", m.display_name);
            }
            Ok(0)
        }
        Command::Grupos => grupos(path, out),
        Command::Status => status(path, out),
    }
}

fn configurar(
    path: &std::path::Path,
    url: Option<&str>,
    nome: Option<&str>,
    out: &mut dyn Write,
) -> Result<i32, NetError> {
    let url = url.map(normalize_base_url).transpose()?;
    let nome = nome.map(validate_display_name).transpose()?;
    let mut identity = match Identity::load(path)? {
        Some(identity) => identity,
        None => {
            let nome = nome
                .as_deref()
                .ok_or_else(|| NetError::Config("faltou o seu nome: --nome \"Nico\"".into()))?;
            Identity::generate(nome)?
        }
    };
    if let Some(url) = url {
        if identity.worker_url.as_deref() != Some(url.as_str()) {
            // A different server has never seen this key.
            identity.worker_url = Some(url);
            identity.registered = false;
        }
    }
    let Some(worker_url) = identity.worker_url.clone() else {
        return Err(NetError::Config(
            "faltou o endereço do servidor: --url https://<worker>".into(),
        ));
    };
    if let Some(nome) = nome {
        if nome != identity.display_name() {
            if identity.registered {
                return Err(NetError::Config(format!(
                    "este PC já está registrado como \"{}\" e o servidor não troca nomes; para usar outro nome, apague o arquivo de identidade (o PC ganhará uma identidade nova)",
                    identity.display_name()
                )));
            }
            identity.set_display_name(&nome)?;
        }
    }
    // Keep the key on disk before any network call.
    identity.save(path)?;

    let mut client = WorkerClient::new(&worker_url, identity)?;
    client.health()?;
    let already = client.identity().registered;
    if !already {
        client.register_device()?;
        client.identity().save(path)?;
    }
    let identity = client.identity();
    if already {
        let _ = writeln!(out, "Este PC já estava registrado.");
    } else {
        let _ = writeln!(out, "PC registrado no servidor.");
    }
    let _ = writeln!(out, "Nome: {}", identity.display_name());
    let _ = writeln!(out, "Id do PC: {}", identity.device_id());
    let _ = writeln!(out, "Próximo passo: duoclip-amigos criar-grupo \"Nome do grupo\" ou duoclip-amigos entrar <codigo>");
    Ok(0)
}

fn grupos(path: &std::path::Path, out: &mut dyn Write) -> Result<i32, NetError> {
    let mut client = ready_client(path)?;
    // One heartbeat as "online, no game, no group chosen": the response carries a snapshot of
    // all my groups. (This makes this PC show as online for about 90 seconds.)
    let request = HeartbeatRequest {
        game: None,
        active_crew: None,
        seated_since_ms: None,
        seq: 1,
        online_since_ms: system_now_ms(),
    };
    let snapshot = client.heartbeat(&request)?;
    if snapshot.crews.is_empty() {
        let _ = writeln!(out, "Você ainda não está em nenhum grupo.");
        return Ok(0);
    }
    let me = client.identity().device_id().to_string();
    let mut changed = false;
    for crew in &snapshot.crews {
        let Some(uuid) = parse_canonical_uuid(&crew.crew_id) else {
            continue;
        };
        let id = CrewId(uuid);
        if client.identity().group_name(id).is_none() {
            client
                .identity_mut()
                .remember_group(id, &short_group_name(id));
            changed = true;
        }
        let nome = client
            .identity()
            .group_name(id)
            .map_or_else(|| id.to_string(), str::to_owned);
        let _ = writeln!(out, "{nome} ({id})");
        let members = client.members(id)?;
        for m in members {
            let device = m.device_id.to_string();
            let online = crew.members.iter().find(|p| p.device_id == device);
            let state = match online {
                Some(p) => match p.game.as_deref() {
                    Some(game) => format!("jogando ({game})"),
                    None => "online".to_owned(),
                },
                None => "offline".to_owned(),
            };
            let you = if device == me { " (você)" } else { "" };
            let _ = writeln!(out, "  {}{you}: {state}", m.display_name);
        }
    }
    if changed {
        client.identity().save(path)?;
    }
    Ok(0)
}

fn status(path: &std::path::Path, out: &mut dyn Write) -> Result<i32, NetError> {
    let _ = writeln!(out, "Arquivo de identidade: {}", path.display());
    let Some(identity) = Identity::load(path)? else {
        let _ = writeln!(
            out,
            "Este PC ainda não foi configurado. Rode: duoclip-amigos configurar --url https://<worker> --nome \"Seu nome\""
        );
        return Ok(0);
    };
    let _ = writeln!(out, "Nome do PC: {}", identity.display_name());
    let _ = writeln!(out, "Id do PC: {}", identity.device_id());
    let _ = writeln!(
        out,
        "Servidor: {}",
        identity.worker_url.as_deref().unwrap_or("(não definido)")
    );
    let _ = writeln!(
        out,
        "Registrado no servidor: {}",
        if identity.registered { "sim" } else { "não" }
    );
    let _ = writeln!(out, "Grupos conhecidos: {}", identity.grupos.len());
    let Some(url) = identity.worker_url.clone() else {
        return Ok(0);
    };
    let client = WorkerClient::new(&url, identity)?;
    match client.health() {
        Ok(()) => {
            let _ = writeln!(out, "Servidor responde: sim");
            Ok(0)
        }
        Err(e) => {
            let _ = writeln!(out, "Servidor responde: não ({e})");
            Ok(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn parses_every_command() {
        let p = parse_args(&args(&[
            "configurar",
            "--url",
            "https://x.dev",
            "--nome",
            "Nico",
        ]))
        .unwrap();
        assert_eq!(
            p.command,
            Command::Configurar {
                url: Some("https://x.dev".into()),
                nome: Some("Nico".into())
            }
        );
        let p = parse_args(&args(&["configurar", "--url=https://x.dev"])).unwrap();
        assert!(matches!(p.command, Command::Configurar { nome: None, .. }));
        assert_eq!(
            parse_args(&args(&["criar-grupo", "Amigos do LoL"]))
                .unwrap()
                .command,
            Command::CriarGrupo {
                nome: "Amigos do LoL".into()
            }
        );
        assert_eq!(
            parse_args(&args(&["criar-grupo", "Amigos", "do", "LoL"]))
                .unwrap()
                .command,
            Command::CriarGrupo {
                nome: "Amigos do LoL".into()
            }
        );
        assert_eq!(
            parse_args(&args(&["convidar"])).unwrap().command,
            Command::Convidar { grupo: None }
        );
        assert_eq!(
            parse_args(&args(&["convidar", "Amigos"])).unwrap().command,
            Command::Convidar {
                grupo: Some("Amigos".into())
            }
        );
        assert_eq!(
            parse_args(&args(&["entrar", "ABCDEFGH23"]))
                .unwrap()
                .command,
            Command::Entrar {
                codigo: "ABCDEFGH23".into()
            }
        );
        assert_eq!(
            parse_args(&args(&["grupos"])).unwrap().command,
            Command::Grupos
        );
        assert_eq!(
            parse_args(&args(&["status"])).unwrap().command,
            Command::Status
        );
        assert_eq!(
            parse_args(&args(&["--help"])).unwrap().command,
            Command::Ajuda
        );
        assert_eq!(
            parse_args(&args(&["ajuda"])).unwrap().command,
            Command::Ajuda
        );
    }

    #[test]
    fn identity_override_works_anywhere() {
        let p = parse_args(&args(&["--identity", "a.json", "status"])).unwrap();
        assert_eq!(p.identity_path, Some(PathBuf::from("a.json")));
        let p = parse_args(&args(&["status", "--identity=b.json"])).unwrap();
        assert_eq!(p.identity_path, Some(PathBuf::from("b.json")));
    }

    #[test]
    fn rejects_bad_usage_in_portuguese() {
        for bad in [
            args(&[]),
            args(&["voar"]),
            args(&["criar-grupo"]),
            args(&["entrar"]),
            args(&["grupos", "extra"]),
            args(&["status", "--url", "https://x.dev"]),
            args(&["configurar", "--url"]),
            args(&["configurar", "--zzz"]),
            args(&["configurar", "sobrou"]),
        ] {
            assert!(parse_args(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn group_resolution() {
        let mut id = Identity::generate("A").unwrap();
        assert!(resolve_group(&id, None).is_err());
        let a = CrewId::new_random();
        id.remember_group(a, "Amigos do LoL");
        assert_eq!(resolve_group(&id, None).unwrap(), a);
        assert_eq!(resolve_group(&id, Some("amigos do lol")).unwrap(), a);
        assert_eq!(resolve_group(&id, Some(&a.to_string())).unwrap(), a);
        assert_eq!(
            resolve_group(&id, Some(&a.to_string().to_uppercase())).unwrap(),
            a
        );
        assert!(resolve_group(&id, Some("outro")).is_err());
        let b = CrewId::new_random();
        id.remember_group(b, "Segundo");
        assert!(resolve_group(&id, None).is_err());
        id.remember_group(CrewId::new_random(), "segundo");
        assert!(resolve_group(&id, Some("Segundo")).is_err());
    }

    #[test]
    fn status_and_commands_without_identity_do_not_touch_the_network() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identidade.json");
        let mut out = Vec::new();
        assert_eq!(execute(&Command::Status, &path, &mut out).unwrap(), 0);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("ainda não foi configurado"));
        // Commands that need a registered PC explain what to do first.
        let err = execute(&Command::Grupos, &path, &mut Vec::new()).unwrap_err();
        assert!(err.to_string().contains("configurar"));
    }

    #[test]
    fn configurar_validates_before_any_network() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identidade.json");
        // http to a non-loopback host, a missing name and a missing url are all local errors.
        let c = |url: Option<&str>, nome: Option<&str>| {
            configurar(&path, url, nome, &mut Vec::new()).unwrap_err()
        };
        assert!(c(Some("http://exemplo.dev"), Some("Nico"))
            .to_string()
            .contains("https"));
        assert!(c(Some("https://exemplo.dev"), None)
            .to_string()
            .contains("--nome"));
        assert!(c(None, Some("Nico")).to_string().contains("--url"));
        assert!(!path.exists() || Identity::load(&path).unwrap().is_some());
    }

    #[test]
    fn status_never_prints_the_secret() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identidade.json");
        let id =
            Identity::from_secret_bytes(duoclip_proto::DeviceId::new_random(), "Nico", &[5u8; 32])
                .unwrap();
        id.save(&path).unwrap();
        let mut out = Vec::new();
        assert_eq!(execute(&Command::Status, &path, &mut out).unwrap(), 0);
        let text = String::from_utf8(out).unwrap();
        use base64::Engine as _;
        assert!(!text.contains(&base64::engine::general_purpose::STANDARD.encode([5u8; 32])));
        assert!(text.contains("Nico"));
    }
}
