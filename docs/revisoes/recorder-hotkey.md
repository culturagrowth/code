# Revisão — atalho do gravador por consulta de teclas

- Revisor: GPT, 08/10/2026; tarefa 21.
- Pedido: `2026-10-08-claude-026-pedido-revisao-recorder-hotkey`.
- Entrega examinada: `53c51defe9029f674c067e107fa81c93cf47e9ff`, `claude/recorder-hotkey`.
- Branch/pasta do revisor: `gpt/revisao-recorder-hotkey`, `worktrees/gpt-revisao-recorder-hotkey`.
- Escopo: diff do `win/app.rs`, contrato no SPEC e fluxo de marcação/extensão de clipes.

## Resultado

Nenhum achado bloqueante. O `GetAsyncKeyState` usado pelo binding retorna `i16`:
`state < 0` consulta o bit alto de tecla atualmente pressionada, sem depender do bit
baixo de evento desde a consulta anterior. Ctrl, Alt e Shift são comparados exatamente
com a configuração; Win aceita qualquer um dos lados. F10 sozinho não dispara Alt+F10,
nem F10 configurado sozinho dispara quando há esses modificadores adicionais.

`pressed()` só retorna verdadeiro na passagem de combinação solta para pressionada.
Uma tecla mantida não repete clipes; a combinação já pressionada na inicialização não
dispara imediatamente. Após soltar e apertar novamente, chama o mesmo `on_hotkey()`
existente, preservando início/extensão e os sons. O loop continua bombeando a sessão,
checando o jogo e processando os resultados dos salvamentos.

A mudança consulta somente o estado da tecla configurada e dos modificadores, sem
gancho de teclado, injeção ou registro de texto digitado. Não amplia fontes de captura.
Não encontrei uso novo de administrador ou mudanças de configuração do Windows.

## Verificações do revisor

- `cargo fmt --all -- --check`: passou.
- `cargo clippy --workspace --all-targets -- -D warnings`: passou.
- `cargo test --workspace`: **613 passaram, 0 falhas, 25 ignorados**.
- `cargo check --workspace --all-targets --target x86_64-pc-windows-gnu`: passou.
- Binding e constantes conferidos no código local do `windows` 0.62.2.
- Integração da tarefa 20 conferida: `82c115e` é ancestral de `dd1ffe5`, preservando código
  e revisão aprovados, inclusive as evidências do teste automático de captura.

Logs locais: `test-output/review-recorder-hotkey/{fmt,clippy,tests,gnu}.log`.
Sem gravação, injeção de teclas ou execução de testes ignorados nesta revisão.
Não há teste automatizado dedicado ao novo poller; sua borda e os modificadores foram
conferidos no código. O F10 dentro do LoL, sem administrador, e o clipe de 40 s a 1080p60
com áudio são **evidências relatadas pelo autor com o usuário**, não uma execução do GPT.
A hipótese sobre raw input do jogo continua não verificada.

## Pontos menores para depois

- **HK-1 (menor, conhecido): conflito do padrão Alt+F10.** `SPEC.md:175` documenta que
  outro aplicativo com o mesmo atalho também pode disparar, inclusive a NVIDIA. Não há
  mais recusa de inicialização por atalho ocupado. Considerar outro padrão futuramente;
  a configuração F10 usada no teste real continua adequada a esse teste.
- **HK-2 (menor, documentação): referências antigas.** `SPEC.md:20`, `:82`, `:124` e
  `:150` ainda descrevem RegisterHotKey/WM_HOTKEY/MOD_NOREPEAT; a nota nova no fim explica
  a mudança. `src/win/app.rs:38` também manteve o comentário do ID removido. Consolidar
  essas descrições quando houver outra edição de documentação, sem bloquear o uso.

**Veredito: aprovado.** Nenhuma rodada de correções solicitada pelos detalhes menores.
