# Quadro de tarefas do DuoClip

Coordena o trabalho entre o **Claude**, o **GPT** e o **usuário**. Regras em [`AGENTS.md`](../AGENTS.md#trabalho-com-mais-de-uma-ia):
só pegue tarefas **livres** ou com o seu nome; uma branch por tarefa; atualize esta tabela ao pegar e ao entregar.

- **Status:** `livre` · `em andamento` · `em revisão` (entregue, esperando revisão/merge) · `concluída` · `bloqueada`
- **Dono:** `Claude` · `GPT` · `Usuário` (testes no PC dele, decisões) · `—` (ninguém)
- **Branch de integração:** `claude/sync-gameplay-clip-app-xpgfwx`

## Em andamento e livres

| # | Tarefa | Fase | Escopo (pastas que pode alterar) | Dono | Status | Branch | Notas |
|---|---|---|---|---|---|---|---|
| 1 | Implementar `duoclip-encode` (conversão NV12 na GPU + H.264 MF hardware + AAC) e validar com NVENC | B1 | `crates/duoclip-encode` | Claude | em andamento | — | Workflow de agentes em 08/10 |
| 2 | Áudio: limiar de salto de ~2 ms no process loopback + revisão adversarial | B1 | `crates/duoclip-audio` | Claude | em andamento | — | Achado do teste real de 08/10 |
| 3 | Revisão adversarial do `duoclip-mux` | B1 | `crates/duoclip-mux` | Claude | em andamento | — | |
| 4 | Revisão adversarial do `duoclip-gamesdb` | B1 | `crates/duoclip-gamesdb` | Claude | em andamento | — | |
| 5 | Crate `duoclip-session` (sessão automática por grupo, até 8) | C | `crates/duoclip-session` | Claude | em andamento | — | Decisão 4.7 da memória |
| 6 | Rodar o `capture_probe` (Desktop Duplication) no PC do usuário, de preferência com um jogo aberto | B2 | — | Usuário | livre | — | Gravar tela exige confirmação |
| 7 | Rodar `audio_probe --capture` num Windows 10 19045 de um amigo, e com `--game-exe` | B1 | — | Usuário | livre | — | |
| 8 | Instalar o PresentMon e medir o impacto do Desktop Duplication (e do Medal, se houver) | B2 | — | Usuário | livre | — | Instalar exige confirmação |
| 9 | `duoclip-capture`: backend `dda_crop` (Desktop Duplication recortado, rotação e HDR) + stub do WGC e do hook | B2 | `crates/duoclip-capture` (novo) | — | livre | — | Precisa de SPEC antes (Claude pode escrever) |
| 10 | Worker: presença por grupo (`POST /v1/presence` com heartbeat e `GET /v1/crews/:crew/presence`) para alimentar a sessão | C | `worker/` | — | livre | — | Boa candidata para o GPT: TypeScript isolado, com testes no vitest |
| 11 | `rust-toolchain.toml` para fixar a versão do Rust | — | raiz | — | livre | — | Decisão do usuário (lints novos quebram o `-D warnings`) |

## Concluídas

| # | Tarefa | Dono | Branch / commit | Resumo |
|---|---|---|---|---|
| — | Primeiro teste real no Windows + crate `duoclip-smoke` + correção do double free no áudio | Claude | `32f98ac` | [relatório](relatorios/teste-local-2026-10-08.md) |
| — | Decisão de grupos e sessões (até 8) registrada | Claude | `e57a7f6` | Seção 4.7 da memória |
