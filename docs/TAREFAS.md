# Quadro de tarefas do DuoClip

Coordena o trabalho entre o **Claude**, o **GPT** e o **usuário**. Regras em [`AGENTS.md`](../AGENTS.md#trabalho-com-mais-de-uma-ia):
só pegue tarefas **livres** ou com o seu nome; uma branch por tarefa; atualize esta tabela ao pegar e ao entregar.
**Quem implementa não revisa:** o revisor é sempre a outra IA, e a revisão fica em [`docs/revisoes/`](revisoes/).

- **Status:** `livre` · `em andamento` · `em revisão` (entregue, esperando o revisor) · `ajustes` (o revisor pediu mudanças) ·
  `aprovada` (pronta para o merge) · `concluída` (já na branch de integração) · `bloqueada`
- **Dono / Revisor:** `Claude` · `GPT` · `Usuário` (testes no PC dele, decisões) · `—` (ninguém ainda)
- **Branch de integração:** `claude/sync-gameplay-clip-app-xpgfwx`

## Em andamento e livres

| # | Tarefa | Fase | Escopo (pastas que pode alterar) | Dono | Revisor | Status | Branch | Notas |
|---|---|---|---|---|---|---|---|---|
| 1 | Implementar `duoclip-encode` (conversão NV12 na GPU + H.264 MF hardware + AAC) e validar com NVENC | B1 | `crates/duoclip-encode` | Claude | GPT | em andamento | `claude/fase-b1` | Workflow de agentes de 08/10. Partes com GPU: o revisor marca como "não verificado" se não puder rodar |
| 2 | Áudio: limiar de salto de ~2 ms no process loopback + revisão do código WASAPI | B1 | `crates/duoclip-audio` | Claude | GPT | em andamento | `claude/fase-b1` | Achado do teste real de 08/10 |
| 3 | Completar e corrigir o `duoclip-mux` | B1 | `crates/duoclip-mux` | Claude | GPT | em andamento | `claude/fase-b1` | |
| 4 | Corrigir o `duoclip-gamesdb` (o hook nunca pode ser escolhido com anti-cheat) | B1 | `crates/duoclip-gamesdb` | Claude | GPT | em andamento | `claude/fase-b1` | |
| 5 | Crate `duoclip-session` (sessão automática por grupo, até 8) | C | `crates/duoclip-session` | Claude | GPT | em revisão | `claude/duoclip-session` | 45 testes. Lógica pura, dá para revisar sem Windows |
| 6 | Rodar o `capture_probe` (Desktop Duplication) no PC do usuário, de preferência com um jogo aberto | B2 | — | Usuário | — | livre | — | Gravar tela exige confirmação |
| 7 | Rodar `audio_probe --capture` num Windows 10 19045 de um amigo, e com `--game-exe` | B1 | — | Usuário | — | livre | — | |
| 8 | Instalar o PresentMon e medir o impacto do Desktop Duplication (e do Medal, se houver) | B2 | — | Usuário | — | livre | — | Instalar exige confirmação |
| 9 | `duoclip-capture`: backend `dda_crop` (Desktop Duplication recortado, rotação e HDR) + stub do WGC e do hook | B2 | `crates/duoclip-capture` (novo) | — | — | livre | — | Precisa de SPEC antes. Depende do Windows real, então rende mais com o Claude implementando |
| 10 | Worker: presença por grupo (`POST /v1/presence` com heartbeat e `GET /v1/crews/:crew/presence`) para alimentar a sessão | C | `worker/` | — | Claude | livre | — | Boa candidata para o GPT: TypeScript isolado, com testes no vitest |
| 11 | `rust-toolchain.toml` para fixar a versão do Rust | — | raiz | — | — | livre | — | Decisão do usuário (lints novos quebram o `-D warnings`) |
| 12 | Revisão retroativa: crate `duoclip-smoke` e a correção do double free no áudio (commit `32f98ac`) | B1 | só `docs/revisoes/` | — | GPT | em revisão | (já na integração) | Entrou antes da regra de revisão cruzada |

## Concluídas

| # | Tarefa | Dono | Revisor | Branch / commit | Resumo |
|---|---|---|---|---|---|
| — | Fase A: proto, crypto, clock, buffer e worker | Claude | Claude (subagentes, antes da regra) | `eccb62a` | Pode ganhar revisão do GPT depois, se o usuário quiser |
| — | Primeiro teste real no Windows + crate `duoclip-smoke` + correção do double free no áudio | Claude | GPT (tarefa 12) | `32f98ac` | [relatório](relatorios/teste-local-2026-10-08.md) |
| — | Decisão de grupos e sessões (até 8) registrada | Claude | — | `e57a7f6` | Seção 4.7 da memória |
| — | Estrutura para duas IAs (`AGENTS.md`, quadro, prompt de delegação) | Claude | — | `41fac5f` | |
