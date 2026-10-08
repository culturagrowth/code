# DuoClip (nome provisório)

App para Windows que grava o jogo de cada amigo e, quando alguém aperta a tecla de clipe, salva
**o POV de todos, sincronizado por um relógio global**, com alguns segundos antes e depois do aperto.
Os clipes são trocados por um **bucket na nuvem (Cloudflare R2)**, sempre criptografados de ponta a ponta.

- Pesquisa e arquitetura: [`docs/pesquisa-app-clipes-sincronizados.md`](docs/pesquisa-app-clipes-sincronizados.md)
- Como OBS, Medal e outros capturam a tela: [`docs/anexo-metodos-de-captura-obs-medal.md`](docs/anexo-metodos-de-captura-obs-medal.md)

## Estrutura

| Pasta | O que é | Plataforma |
|---|---|---|
| `crates/duoclip-proto` | Mensagens entre os apps (pedido de clipe, confirmações, pings de relógio), validação e nomes das chaves no bucket | Qualquer |
| `crates/duoclip-clock` | **Relógio Global DuoClip**: relógio monotônico disciplinado para UTC (SNTP/NTS) + refino P2P | Qualquer |
| `crates/duoclip-buffer` | Buffer de replay em RAM + "fixar e coletar" (gravar antes **e depois** do aperto) | Qualquer |
| `crates/duoclip-crypto` | Criptografia ponta a ponta dos pedaços do clipe e do manifesto + agrupamento em blocos | Qualquer |
| `worker/` | Cloudflare Worker: dispositivos, grupos de amigos, URLs assinadas do R2, limpeza automática | Cloudflare |
| *(próximas fases)* | Captura (Desktop Duplication / WGC, hook futuro), áudio WASAPI, encoder, app Tauri, editor | Windows |

Cada crate tem um `SPEC.md` com o contrato que a implementação segue.

## Como compilar e testar

```bash
# Rust (crates independentes de plataforma)
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

# Checagem cruzada para Windows (sem linkar)
rustup target add x86_64-pc-windows-gnu
cargo check --workspace --target x86_64-pc-windows-gnu

# Worker
cd worker && npm install && npm run typecheck && npm test
```
