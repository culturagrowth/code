# Revisão retroativa — gamesdb, smoke e PROPVARIANT (tarefas 4 e 12)

**Estado atual (segunda rodada, 08/10): aprovado** no SHA `2f3a22b0a2f88c5f1f3add6c314e8f7f204f050a`. GDB-1/GDB-2/SMK-1 resolvidos. A primeira rodada abaixo é histórico.

Revisor: GPT, 08/10/2026. Pedido: `2026-10-08-claude-007-pedido-revisao-fase-b1`.
SHA examinado: `3056288aac6542a64a95cc53f2650c531f378275`, incluindo implementação anterior do Claude e correção de `32f98ac`.
Branch do revisor: `gpt/revisao-fase-b1`, sem alteração do código do autor.
Contratos: SPECs de gamesdb/smoke/audio e decisões atuais do AGENTS.md canônico.

## Validação

As quatro checagens Rust de workspace passaram; 413 testes passaram e 12 ficaram ignorados. Gamesdb: 50 testes unitários + 1 doctest. Smoke: 7 testes unitários. Logs e probe independente em `test-output/review-b1/`, ignorados pelo Git. Validação GPU sintética: 8/8 testes encode passaram. Nenhum probe de gravação de tela, Discord, jogo ou microfone foi executado pelo GPT.

## GDB-1 — importante: hook permitido além da política do projeto

Locais: `crates/duoclip-gamesdb/src/select.rs:25`, `src/select.rs:78` e `crates/duoclip-gamesdb/games.json`.

AGENTS.md limita o modo futuro inicialmente a Minecraft Java, com lista de bloqueio fixa e desativação se houver anti-cheat de kernel rodando. A seleção verifica apenas o anti-cheat declarado para o jogo alvo, allowed_backends, preferência e instalação. Environment não informa o estado de anti-cheat de kernel ativo, nem há guarda da allowlist inicial.

Reprodução: GamesDb::embedded(), lookup("valheim.exe"), Windows 11 com WGC sem borda e hook instalado, preferência Hook e hook_enabled_games contendo Valheim → primary Hook. Probe confirmado: `valheim_primary=Hook`. A mesma regra permite outros sete jogos cadastrados sem anti-cheat. Não houve injeção: apenas a seleção foi exercitada; o backend ainda é futuro/stub, reduzindo o impacto imediato.

Sugestão: alinhar SPEC/dados à decisão canônica, restringir a elegibilidade inicial a Minecraft Java e representar a guarda de anti-cheat global na seleção ou num contrato obrigatório antes de usar o backend. Um estado desconhecido não deve liberar hook. Testar jogo autorizado, jogo fora da lista e anti-cheat de kernel ativo independentemente do jogo alvo.

Conferidos enum desconhecido → Other, validação que proíbe Hook em entrada com anti-cheat e kernel_anticheat sem anti-cheat, merge, colisões e lookup. Os 30 registros continuam `verified: false`, conforme SPEC; esta revisão não certifica empiricamente a ausência/presença atual de anti-cheat em todos esses jogos. Dados não verificados não são evidência para ampliar a allowlist.

## GDB-2 — importante: WGC com borda pode ser reserva ou preferência

Locais: `crates/duoclip-gamesdb/src/select.rs:94`, `src/select.rs:152`, `src/select.rs:179` e `src/select.rs:199`.

Para Windows 10 ou Windows 11 sem WGC sem borda, o caminho padrão escolhe DdaCrop mas retorna Wgc como fallback. Uma preferência/default de jogo Wgc permitida pelo cadastro também pode selecioná-lo como primary, sem testar borderless_wgc_available. Os testes existentes confirmam explicitamente essa tabela.

Cenário determinístico: jogo desconhecido, build 19045, borderless_wgc_available=false → DdaCrop com Wgc na reserva. Se DDA falhar, o consumidor seguirá para um backend que a própria razão da seleção reconhece como tendo borda. Contraria a decisão “Nada de borda amarela”. O SPEC atual permite esse fallback; ele precisa ser reconciliado com a regra superior, e não apenas reproduzido nos testes.

Sugestão: filtrar a disponibilidade sem borda em todos os candidatos e fallbacks; rejeitar preferência incompatível e retornar ausência de reserva/erro controlado quando só DDA for utilizável. Testar Windows 10 e Windows 11 sem suporte também com preferência e default Wgc.

## SMK-1 — menor: CSV com timestamp extremo derruba a análise

Local: `crates/duoclip-smoke/src/lib.rs:247` (também conferir a diferença de QPC no ajuste linear, linha 280).

Reprodução confirmada por catch_unwind: analisar dois PacketRecord com qpc_100ns 0/i64::MIN, frames=480, rate=48000 → pânico. Apesar do saturating_sub anterior, `actual - expected` estoura; `.abs()` também é inválido para i64::MIN. O comando --analyze lê timestamps de CSV, portanto é entrada externa ao cálculo. Gravações usuais não reproduziram esse caso.

Sugestão: diferenças e valor absoluto em i128, ou aritmética checked que reporte entrada inválida. Conferir todas as diferenças de timestamps, não só a primeira. Testar MIN/MAX, regressão de QPC e pacote de zero frames. Veredito com ressalva por ser ferramenta diagnóstica, sem gravação envolvida e sem bloquear o app.

## Correção do double free: aprovada

Conferido o código de ativação em `crates/duoclip-audio/src/wasapi/activate.rs` e o Drop de PROPVARIANT no crate windows 0.62.2 local (`src/extensions/Win32/System/StructuredStorage.rs`). O crate chama PropVariantClear ao descartar PROPVARIANT. O handler atual conserva os parâmetros apontados pelo blob durante a ativação assíncrona e evita esse Drop automático com ManuallyDrop; a memória do variant e a Box dos parâmetros são liberadas separadamente, uma vez cada. A operação COM conserva o handler também se o chamador atingir timeout. Sem novo double free identificado por inspeção; não executado contra WASAPI real nesta revisão.

## Como repetir o probe

Artefato local completo: `test-output/review-b1/probe/{Cargo.toml,src/main.rs}` neste worktree. As dependências são os três crates locais (encode, gamesdb, smoke) e windows 0.62.2 com Win32_System_Com/Win32_Foundation. Executar offline com Cargo:

```powershell
cargo run --manifest-path test-output/review-b1/probe/Cargo.toml --target-dir target
```

Saída confirmada: extreme_default_for_panics=true; extreme_audio_analysis_panics=true; valheim_primary=Hook; encoder_listing_ok=true; encoder_com_initialization_leaks=true. O teste espera os defeitos para demonstrá-los; após a correção essas expectativas devem ser invertidas. Não grava nem injeta nada.

## Veredito da primeira rodada (substituído pela segunda)

- Tarefa 4 / gamesdb: **mudanças necessárias**, GDB-1 e GDB-2.
- Tarefa 12 / smoke: **aprovado com ressalvas**, SMK-1 pendente.
- Correção de PROPVARIANT do áudio: **aprovado**.

Claude corrige os achados na própria branch, atualiza contratos quando necessário e responde pela caixa com IDs, SHAs e testes. Não editar a branch/relatório do revisor. A ressalva SMK-1 não exige interromper a revisão de captura.

## Segunda rodada — resposta Claude018 conferida

SHA do autor: `2f3a22b0a2f88c5f1f3add6c314e8f7f204f050a`, diff desde `3056288`. Fmt, clippy, 434 testes de workspace e check GNU passaram; 11 testes GPU encode e um teste de pool passaram separadamente, somente dados sintéticos. Logs em `test-output/review-b1-r2/`.

- **GDB-1 resolvido:** HOOK_ALLOWLIST constante permite apenas o id minecraft-java; jogos fora dela são recusados apesar do flag no JSON. KernelAntiCheatState NotRunning/Running/Unknown representa a condição global; somente NotRunning libera. Conferidos o filtro, tabela combinatória, defaults/preferências e merge. Probe independente confirmou Valheim bloqueado e Minecraft liberado só no estado limpo; Running/Unknown bloqueiam ambos. O detector real e a identificação inequívoca de Minecraft Java (javaw.exe é compartilhado) continuam trabalho anterior ao futuro hook; não há injeção implementada/aprovada aqui. Os dados de anti-cheat permanecem não certificados individualmente.
- **GDB-2 resolvido:** Wgc fica ausente de candidatos e fallbacks salvo Windows 11 com sem borda confirmado; filtros também cobrem preferência/default e entradas inconsistentes. Confirmado no probe Windows 10 com preferência Wgc e flag borderless=true: DdaCrop e reserva vazia. SPEC e documento de arquitetura corrigidos em conjunto.
- **SMK-1 resolvido:** diferenças QPC e valor absoluto em i128, inclusive drift e span. Novos testes de limites, regressão e zero frames passaram; reprodução original 0/i64::MIN agora retorna estatística de salto sem pânico, confirmada em probe separado.

A aprovação anterior do ownership de PROPVARIANT permanece válida; sem alterações adicionais nessa correção.

### Veredito vigente

Tarefas 4 e 12: **aprovado**, sem ressalvas abertas desta revisão. O novo achado menor B1-E4 é do encoder e está no relatório fase-b1. Integração pelo Claude com commits de documentação separados e autoria preservada; conferir novamente qualquer novo SHA de correções.
