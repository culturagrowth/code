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
| 1 | Implementar `duoclip-encode` (conversão NV12 na GPU + H.264 MF hardware + AAC) e validar com NVENC | B1 | `crates/duoclip-encode` | Claude | GPT | concluída | `claude/fase-b1` | GPT conferiu 2f3a22b: B1-E1/E2/E3 fechados; aprovado com ressalva menor B1-E4. Relatório 1d522dd / GPT018; 434 testes + 11 GPU + 1 pool passaram Integrada no merge `235d19f`. B1-E4 (menor) fica para depois |
| 2 | Áudio: limiar de salto de 2 ms no process loopback (+ revisão do código WASAPI, que nunca teve revisão de outra IA) | B1 | `crates/duoclip-audio` | Claude | GPT | concluída | `claude/fase-b1` | GPT aprovou áudio em 3056288; relatório 25d6e2d. Merge conjunto aguarda ajustes do encode Integrada no merge `235d19f`. B1-E4 (menor) fica para depois |
| 3 | `duoclip-mux`: conferido contra o SPEC (estava completo) e testes com ffprobe reforçados | B1 | `crates/duoclip-mux` | Claude | GPT | concluída | `claude/fase-b1` | GPT aprovou: 39 testes, FFmpeg executado. Relatório 25d6e2d; merge conjunto aguarda encode Integrada no merge `235d19f`. B1-E4 (menor) fica para depois |
| 4 | Revisão do `duoclip-gamesdb` (o hook nunca pode ser escolhido com anti-cheat; dados de anti-cheat) | B1 | só `docs/revisoes/` | GPT | GPT | concluída | `gpt/revisao-fase-b1` | GDB-1/GDB-2 fechados em 2f3a22b; relatório 1d522dd / GPT018, aprovado Integrada no merge `235d19f`. B1-E4 (menor) fica para depois |
| 5 | Crate `duoclip-session` (sessão automática por grupo, até 8) | C | `crates/duoclip-session` | Claude | GPT | concluída | `claude/duoclip-session` → merge `6379f26` | Aprovado pelo GPT (`gpt/revisao-duoclip-session:docs/revisoes/duoclip-session.md`) |
| 6 | Rodar o `capture_probe` (Desktop Duplication) no PC do usuário, de preferência com um jogo aberto | B2 | — | Usuário | — | livre | — | Gravar tela exige confirmação |
| 7 | Rodar `audio_probe --capture` num Windows 10 19045 de um amigo, e com `--game-exe` | B1 | — | Usuário | — | livre | — | |
| 8 | Instalar o PresentMon e medir o impacto do Desktop Duplication (e do Medal, se houver) | B2 | — | Usuário | — | livre | — | Instalar exige confirmação |
| 9 | `duoclip-capture`: Desktop Duplication recortado (rotação e HDR) + stubs de WGC e hook | B2 | `crates/duoclip-capture` | Claude | GPT | concluída | `claude/duoclip-capture` | GPT conferiu 9b62edb: CAP-1 a CAP-4 respondidos; aprovado com ressalvas de oclusão/polling, sem garantia de isolamento absoluto. Relatório c25d97e / GPT019; 476 testes + 1 teste sem captura passaram. B1 aprovada com ressalva menor; aguarda integração pelo Claude Integrada no merge `66f6cc3` |
| 10 | Worker: presença por grupo (heartbeat que devolve o retrato de todos os grupos; `GET /v1/crews/:crew/presence`) | C | `worker/` | GPT | Claude | concluída | `gpt/worker-presenca` → merge `efc1030` | Aprovado na 2ª rodada (`deadb4c`): `seated_since_ms`, AppClock, 30 s/90 s, sem índice. 372 testes Worker na integração |
| 11 | `rust-toolchain.toml` para fixar a versão do Rust | — | raiz | — | — | livre | — | Decisão do usuário (lints novos quebram o `-D warnings`) |
| 12 | Revisão retroativa: crate `duoclip-smoke` e a correção do double free no áudio (commit `32f98ac`) | B1 | só `docs/revisoes/` | GPT | GPT | concluída | `gpt/revisao-fase-b1` | SMK-1 fechado em 2f3a22b; smoke e double free aprovados. Relatório 1d522dd / GPT018 Integrada no merge `235d19f`. B1-E4 (menor) fica para depois |
| 13 | Configurar os bindings R2/D1 e validar upload/download no R2 real | C | `worker/` | GPT | Claude | concluída | `gpt/worker-r2` → merge `93fcffa` | Aprovado na 2ª rodada (`6234758`). Pendentes: migrações remotas, regra de ciclo de vida pelo painel e deploy |
| 14 | Configurar comunicação direta por arquivos e organizar os worktrees na pasta principal | — | `AGENTS.md`, `CLAUDE.md`, `.gitignore`, `docs/` | GPT | Claude | concluída | `gpt/comunicacao-agentes` → merge `5a1e69b` | Aprovado com ressalvas (`claude/revisao-comunicacao:docs/revisoes/comunicacao-agentes.md`); ressalvas na tarefa 15 |
| 15 | Protocolo, 2ª rodada: pasta principal fixa na integração, revisão em worktree próprio, relatório em `<revisor>/revisao-<tarefa>`, memória sem estado volátil, numeração das mensagens | — | `AGENTS.md`, `docs/` | GPT | Claude | concluída | `gpt/protocolo-2` → merge `d7c8013` | Aprovado (`claude/revisao-protocolo-2:docs/revisoes/protocolo-2.md`) |
| 17 | Adaptador de presença Worker → SessionManager: retratos completos, remoção de ausentes, 30 s/TTL 90 s, `online_since_ms` crescente entre execuções, heartbeat só com jogo/sessão ativa | C | `crates/duoclip-presence` (novo) + `apply_snapshot` em `crates/duoclip-session` | Claude | GPT | em revisão | `claude/presence-adapter` | Lógica pura, sem rede (transporte HTTP/assinatura fica para a Fase C) Entregue `0e6e22f` (567 testes) |
| 16 | Decisão do usuário sobre o bucket `povclip` | C | — | Usuário | — | concluída | — | 08/10: o bucket **não guarda mais nada** de outro app; a regra de ciclo de vida em `clips/` (expirar em 3 dias, abortar multipart em 1 dia) pode ser aplicada. Aplicar pelo Wrangler falhou (`code 10042`, "enable R2 through the Dashboard"); fazer pelo painel ou junto do deploy |
| 18 | Revisão cruzada da captura implementada pelo Claude (tarefa 9) | B2 | só `docs/revisoes/` | GPT | GPT | concluída | `gpt/revisao-duoclip-capture` | Segunda rodada concluída em 9b62edb: relatório c25d97e / GPT019, aprovado com ressalvas. Captura real não executada pelo GPT; autor mantém implementação na tarefa 9 Integrada no merge `66f6cc3` |
| 19 | Gravador local utilizável (`duoclip-recorder`): grava jogo + áudio (jogo, Discord, mic) e salva MP4 no atalho, com configuração por pessoa (qualidade, encoder, segundos antes/depois, atalho, aviso sonoro) | B3 | `crates/duoclip-recorder` (novo) | Claude | GPT | em andamento | `claude/recorder` | Primeiro programa para o usuário usar. Testes que gravam tela/áudio só com confirmação |

| 19 | Comando de teste do fluxo de grupo com dois participantes: cadastro, convite e presença | C | `worker/tools/crew-smoke.ts`, teste correspondente, scripts e documentação Worker | GPT | Claude | em revisão | `gpt/crew-smoke` | Entrega d871a2f / GPT022; comando test:crew, fluxo HTTP local passou, 375 testes Worker + typecheck; sem captura/R2. Não altera tarefa 17 |

## Concluídas

| # | Tarefa | Dono | Revisor | Branch / commit | Resumo |
|---|---|---|---|---|---|
| — | Fase A: proto, crypto, clock, buffer e worker | Claude | Claude (subagentes, antes da regra) | `eccb62a` | Pode ganhar revisão do GPT depois, se o usuário quiser |
| — | Primeiro teste real no Windows + crate `duoclip-smoke` + correção do double free no áudio | Claude | GPT (tarefa 12) | `32f98ac` | [relatório](relatorios/teste-local-2026-10-08.md) |
| — | Decisão de grupos e sessões (até 8) registrada | Claude | — | `e57a7f6` | Seção 4.7 da memória |
| — | Estrutura para duas IAs (`AGENTS.md`, quadro, prompt de delegação) | Claude | — | `41fac5f` | |
