# Quadro de tarefas do DuoClip

Coordena o trabalho entre o **Claude**, o **GPT** e o **usuário**. Regras em [`AGENTS.md`](../AGENTS.md#trabalho-com-mais-de-uma-ia):
só pegue tarefas **livres** ou com o seu nome; uma branch por tarefa; atualize esta tabela ao pegar e ao entregar.
**Este arquivo é editado sempre direto na branch de integração** (commits pequenos), nunca nas branches de tarefa, para não gerar conflito.
**Quem implementa não revisa:** o revisor é sempre a outra IA, e a revisão fica em [`docs/revisoes/`](revisoes/).

- **Status:** `livre` · `em andamento` · `em revisão` (entregue, esperando o revisor) · `ajustes` (o revisor pediu mudanças) ·
  `aprovada` (pronta para o merge) · `concluída` (já na branch de integração) · `bloqueada`
- **Dono / Revisor:** `Claude` · `GPT` · `Usuário` (testes no PC dele, decisões) · `—` (ninguém ainda)
- **Branch de integração:** `claude/sync-gameplay-clip-app-xpgfwx`

## Em andamento e livres

| # | Tarefa | Fase | Escopo (pastas que pode alterar) | Dono | Revisor | Status | Branch | Notas |
|---|---|---|---|---|---|---|---|---|
| 1 | Implementar `duoclip-encode` (conversão NV12 na GPU + H.264 MF hardware + AAC) e validar com NVENC | B1 | `crates/duoclip-encode` | Claude | GPT | em revisão | `claude/fase-b1` | 33 testes + 8 de GPU (`--ignored`) passando na RTX 5060 Ti. Sem a GPU, o revisor marca os testes de GPU como "não verificado" |
| 2 | Áudio: limiar de salto de 2 ms no process loopback (+ revisão do código WASAPI, que nunca teve revisão de outra IA) | B1 | `crates/duoclip-audio` | Claude | GPT | em revisão | `claude/fase-b1` | 36 testes |
| 3 | `duoclip-mux`: conferido contra o SPEC (estava completo) e testes com ffprobe reforçados | B1 | `crates/duoclip-mux` | Claude | GPT | em revisão | `claude/fase-b1` | 39 testes; precisa de ffmpeg/ffprobe no PATH |
| 4 | Revisão do `duoclip-gamesdb` (o hook nunca pode ser escolhido com anti-cheat; dados de anti-cheat) | B1 | só `docs/revisoes/` | — | GPT | em revisão | (já na integração) | Implementado pelo Claude em 07/10, nunca revisado. Se houver achados, o Claude corrige |
| 5 | Crate `duoclip-session` (sessão automática por grupo, até 8) | C | `crates/duoclip-session` | Claude | GPT | em revisão | `claude/duoclip-session` | 45 testes. Lógica pura, dá para revisar sem Windows |
| 6 | Rodar o `capture_probe` (Desktop Duplication) no PC do usuário, de preferência com um jogo aberto | B2 | — | Usuário | — | livre | — | Gravar tela exige confirmação |
| 7 | Rodar `audio_probe --capture` num Windows 10 19045 de um amigo, e com `--game-exe` | B1 | — | Usuário | — | livre | — | |
| 8 | Instalar o PresentMon e medir o impacto do Desktop Duplication (e do Medal, se houver) | B2 | — | Usuário | — | livre | — | Instalar exige confirmação |
| 9 | `duoclip-capture`: backend `dda_crop` (Desktop Duplication recortado, rotação e HDR) + stub do WGC e do hook | B2 | `crates/duoclip-capture` (novo) | Claude | GPT | em andamento | `claude/duoclip-capture` | SPEC pelo Claude; testes que capturam a tela só rodam com a confirmação do usuário |
| 10 | Worker: presença por grupo (`POST /v1/presence` com heartbeat e `GET /v1/crews/:crew/presence`) para alimentar a sessão | C | `worker/` | GPT | Claude | ajustes | `gpt/worker-presenca` | Revisado `48c0f8a`: **mudanças necessárias** — `seated_since_ms` no contrato, `online_since_ms` no relógio global, orçamento de gravações do D1. Relatório: `claude/revisao-worker-presenca:docs/revisoes/worker-presenca.md` |
| 11 | `rust-toolchain.toml` para fixar a versão do Rust | — | raiz | — | — | livre | — | Decisão do usuário (lints novos quebram o `-D warnings`) |
| 12 | Revisão retroativa: crate `duoclip-smoke` e a correção do double free no áudio (commit `32f98ac`) | B1 | só `docs/revisoes/` | — | GPT | em revisão | (já na integração) | Entrou antes da regra de revisão cruzada |
| 13 | Configurar os bindings R2/D1 e validar upload/download no R2 real | C | `worker/` | GPT | Claude | em revisão | `gpt/worker-r2` | Ajustes em `6234758`: README/SPEC/exemplo adotam `povclip`; usuário confirmou uso exclusivo; regra pelo painel documentada. Node mínimo já declarado. Aguarda conferência de `claude/revisao-worker-r2:docs/revisoes/worker-r2.md` |
| 14 | Configurar comunicação direta por arquivos e organizar os worktrees na pasta principal | — | `AGENTS.md`, `CLAUDE.md`, `.gitignore`, `docs/` | GPT | Claude | concluída | `gpt/comunicacao-agentes` → merge `5a1e69b` | Aprovado com ressalvas (`claude/revisao-comunicacao:docs/revisoes/comunicacao-agentes.md`); ressalvas na tarefa 15 |
| 15 | Protocolo, 2ª rodada: pasta principal sempre na integração (a caixa some ao trocar de branch), revisor usa worktree próprio (dono diferente no Windows), relatório na branch `<revisor>/revisao-<tarefa>`, memória sem estado volátil | — | `AGENTS.md`, `docs/COMUNICACAO-AGENTES.md`, `docs/revisoes/README.md`, `docs/PROMPT-DELEGAR-TAREFA.md` | GPT | Claude | livre | — | Achados 1–5 da revisão da tarefa 14 |
| 16 | Decisão do usuário sobre o bucket `povclip` | C | — | Usuário | — | concluída | — | 08/10: o bucket **não guarda mais nada** de outro app; a regra de ciclo de vida em `clips/` (expirar em 3 dias, abortar multipart em 1 dia) pode ser aplicada. Aplicar pelo Wrangler falhou (`code 10042`, "enable R2 through the Dashboard"); fazer pelo painel ou junto do deploy |

## Concluídas

| # | Tarefa | Dono | Revisor | Branch / commit | Resumo |
|---|---|---|---|---|---|
| — | Fase A: proto, crypto, clock, buffer e worker | Claude | Claude (subagentes, antes da regra) | `eccb62a` | Pode ganhar revisão do GPT depois, se o usuário quiser |
| — | Primeiro teste real no Windows + crate `duoclip-smoke` + correção do double free no áudio | Claude | GPT (tarefa 12) | `32f98ac` | [relatório](relatorios/teste-local-2026-10-08.md) |
| — | Decisão de grupos e sessões (até 8) registrada | Claude | — | `e57a7f6` | Seção 4.7 da memória |
| — | Estrutura para duas IAs (`AGENTS.md`, quadro, prompt de delegação) | Claude | — | `41fac5f` | |
