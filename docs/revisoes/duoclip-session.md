# Revisão — tarefa 5: gerenciador de sessões por grupo

- Implementado por: Claude · Revisado por: GPT · Data: 2026-10-08.
- Branch entregue: `claude/duoclip-session`.
- SHA revisado: `2ec1b6d6dffad67f653fe88f64c78c687a6d00bb`; diff desde `a803b09`.
- Branch do revisor: `gpt/revisao-duoclip-session`, criada no SHA exato acima.
- Worktree: `C:\Users\bolad\Projetos\duoclip\worktrees\gpt-revisao-duoclip-session`.
- Pedido: `2026-10-08-claude-008-pedido-revisao-duoclip-session`.

## Checagens executadas

Todas passaram, com as dependências já disponíveis e `CARGO_NET_OFFLINE=true`:

- `cargo fmt --all -- --check`.
- `cargo clippy --workspace --all-targets -- -D warnings`.
- `cargo test --workspace`, incluindo os doctests; nenhuma falha.
- `cargo check --workspace --all-targets --target x86_64-pc-windows-gnu`.

Os **45 testes de `duoclip-session`** passaram: 38 casos de sessão, 6 simulações com vários dispositivos e
1 teste de propriedades. Os testes ignorados de outros crates permaneceram ignorados. Não houve captura
de tela, microfone ou áudio do sistema, nem execução dos testes de hardware. O Worker não mudou nesta
entrega; checagens dele não fazem parte desta revisão.

## Escopo e resultado da leitura

Conferi o SPEC, a implementação inteira e os testes de isolamento, fila, reinício, escolha de grupo,
requisições e propriedades. O diff adiciona o crate e seu registro no `Cargo.lock`; não altera o código
dos outros crates. Não modifiquei a implementação nem o worktree do autor.

- A associação ao grupo é conferida antes de aceitar presença, e a seleção filtra membros, jogo,
  prazo de presença e compromisso com outro grupo. Um candidato ainda indeciso não recebe clipes.
- Apenas `Active` oferece alvos e aceita pedidos de participantes do mesmo grupo; `Queued`, `Idle`,
  `NeedsChoice`, outros grupos e pedidos com o próprio identificador são rejeitados.
- Remover um membro ou sair de um grupo reavalia imediatamente os alvos. `clip_targets` devolve uma
  cópia independente, preservando o conjunto escolhido para um pedido já enviado.
- O limite de vagas usa os dados anunciados de assento, início da execução e identificador. As
  simulações verificam acordo entre dispositivos, promoção da fila e ausência de participação em
  dois grupos após as trocas necessárias. A divergência transitória está documentada no SPEC.
- Ordenação por `(online_since_ms, seq)`, limites de dispositivos e jogos e aritmética saturada estão
  coerentes com o contrato. O crate proíbe `unsafe`; a entrada aleatória e os extremos temporais
  exercitados nos testes não causaram pânico.

## Achados

Nenhum achado crítico, importante ou menor que exija correção nesta entrega.

## Limites e integração futura

Estas checagens cobrem a lógica portátil. Não verificam o transporte real, autenticação das presenças,
persistência do relógio entre execuções ou comportamento sob perda de rede. O SPEC atribui esses pontos
ao transporte e explicita os limites de replay, relógio e convergência.

A tarefa **17**, já no quadro canônico, deve configurar `presence_ttl_ms = 90_000` ao conectar este crate
ao Worker com heartbeat de 30 s. O padrão portátil é 30 s (`src/lib.rs:72`) e o exemplo do contrato fala
em anúncios a cada 5–10 s (`SPEC.md:129`); usar o padrão com o transporte de 30 s deixaria pouca margem
para atraso. O adaptador deve aplicar retratos completos, retirar ausentes, preservar os pares de
execução/sequência, garantir `online_since_ms` crescente entre execuções e anunciar mudanças de
compromisso e assento imediatamente. Essa integração está fora da tarefa 5 e não foi declarada pronta.

## Veredito

**Aprovado.** A lógica entregue atende ao SPEC e passou por todas as checagens obrigatórias.
Claude pode integrar o SHA revisado e incluir este relatório por commit separado de documentação,
preservando a autoria. A tarefa 17 continua necessária para ligar as sessões ao transporte do Worker.
