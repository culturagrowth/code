# Revisão — gravador local (`duoclip-recorder`)

- Revisor: GPT, 08/10/2026.
- Tarefa: 20; pedido `2026-10-08-claude-024-pedido-revisao-recorder`.
- Entrega examinada: `claude/recorder`, `6b70abb26783c5e78f457b42caf0a8862b416dc5`.
- Revisão isolada: `gpt/revisao-recorder`, em `worktrees/gpt-revisao-recorder`.
- Escopo: SPEC e diff do gravador, configuração, detecção, captura, mistura de áudio, atalho,
  buffer e salvamento de MP4. Prioridade ao uso normal entre amigos, conforme decisão 19.

## Resultado

Nenhum achado bloqueante na revisão do código. O fluxo normal mantém a captura vinculada à
janela escolhida, usa fontes de áudio configuradas sem substituir falhas por captura geral
do sistema, estende o clipe em apertos repetidos e salva em arquivo temporário antes de
renomear o MP4. O microfone vem desligado por padrão. O encerramento normal drena as fontes
e aguarda os salvamentos. A execução com hardware abaixo confirmou parte do fluxo;
a captura da imagem real ainda falta validar.

## Verificações executadas sem gravação

- `cargo fmt --all -- --check`: passou.
- `cargo clippy --workspace --all-targets -- -D warnings`: passou.
- `cargo test --workspace`: **613 passaram, 0 falharam, 25 ignorados**.
- `cargo check --workspace --all-targets --target x86_64-pc-windows-gnu`: passou.
- CLI `--help` e `--check-config --config <arquivo inexistente>`: passaram;
  a segunda não criou o arquivo de configuração.

Logs locais: `test-output/review-recorder/{fmt,clippy,tests,gnu,help,config}.log`.
Nesta etapa não foram executados testes ignorados nem gravação. O teste autorizado
posteriormente está registrado abaixo; áudio de processos e microfone não foram capturados.

## Achado menor, sem bloquear o teste

**REC-1 — o teste de hardware não confirma o conteúdo visual/sonoro.**
Em `crates/duoclip-recorder/tests/recorder_hw.rs:240`, a obtenção de primeiro plano é apenas
impressa. As verificações a partir da linha 301 conferem codecs, duração e decodificação,
sem validar as cores/movimento nem o tom de 440 Hz. Um MP4 com quadros de proteção pretos ou
áudio silencioso poderia passar essas verificações. Sugestão: conferir o conteúdo do MP4
produzido ao executar este teste; reforçar suas asserções futuramente, se necessário.

## Teste real executado — validação parcial

O usuário confirmou em 08/10/2026: "pode rodar o teste". Também reafirmou que pontos finos
devem ser anotados com prioridade baixa, conforme decisão 19. Nenhuma mudança de código
foi feita para esses pontos.

Executado apenas o teste autorizado, em Windows com a RTX 5060 Ti:

```text
cargo test -p duoclip-recorder --test recorder_hw -- --ignored --exact records_test_window_and_saves_clip --nocapture --test-threads=1
```

**1 teste passou, 0 falhas**, em 11,45 s após a compilação. A janela própria foi criada,
mas `test window is the foreground window: false`; o gravador emitiu quadros de proteção
escuros. Não afrouxamos essa proteção nem usamos o modo de teste que ignora primeiro plano.

- Encoder efetivo: `NVIDIA H.264 Encoder MFT` (hardware).
- Clipe coletado sem lacunas ou truncamento indicado pelo buffer.
- MP4 salvo, **1280×720, 30 fps**, H.264 + AAC estéreo 48 kHz, **3,031 s**, 110.850 bytes.
- Decodificação das duas faixas com `ffmpeg -xerror`: sem erros.
- Áudio analisado depois de decodificar: RMS 0,1414; frequência estimada por cruzamentos
  positivos de zero **440,005 Hz**, confirmando o tom sintético, sem silêncio.
- Inspeção visual de um quadro e análise de todos os **90 quadros apresentados**,
  reduzidos a 64×36: uma única imagem distinta, canais de 0 a 13. As cores e o movimento
  **não foram capturados**. Isso confirma a limitação REC-1, não um teste completo de imagem.

Arquivo: `test-output/recorder/recorder-e2e.mp4` deste worktree.
Evidências adicionais: `test-output/review-recorder/hardware.log`, `frame-1s.png`,
`audio.wav`, `verify_media.py` e `media-verification.json` (todos locais, ignorados pelo Git).
SHA-256 do MP4: `acf64de1ef67bb858846f0619807516828c86689b975262888cae5609a9ff1ee`.

Para validar a imagem, repetir o teste mantendo a janela colorida em primeiro plano.
A confirmação já concedida cobre a repetição desse mesmo teste; não precisa pedi-la de novo.

### Segunda tentativa e diagnóstico do primeiro plano

A pedido do usuário, o mesmo teste foi repetido, desta vez com PTY. Novamente passou
(1 teste, 0 falhas), mas a janela não obteve primeiro plano. MP4 de 3,018 s, 132.560 bytes,
áudio de 440,005 Hz e 90 quadros apresentados escuros idênticos na análise reduzida.
SHA-256: `0360fe5efdc4173eceaf0e44457f6b38e7df1a39f3090a42fb3fb47377657199`.
Log: `test-output/review-recorder/hardware-tentativa-2.log`.

Consulta sem captura às APIs do Windows identificou `WinSta0`, desktop
`CodexSandboxDesktop-…` e `GetForegroundWindow() == NULL`. Isso explica a ausência de
primeiro plano nas tentativas deste ambiente: os comandos estão num desktop isolado do
Codex. Não é evidência de falha do gravador no desktop normal do usuário. Não tentamos
remover o isolamento, trocar desktop ou contornar a regra de foco do gravador.

Próximo passo concreto: executar no PowerShell normal do Windows o script local
`test-output/review-recorder/rodar-teste-primeiro-plano.ps1` deste worktree, que roda somente
o mesmo teste, offline, e salva `hardware-desktop-normal.log`. O script foi conferido com
o parser do PowerShell, mas não executado dentro do sandbox, onde repetiria a mesma
limitação. A primeira tentativa foi preservada como
`test-output/review-recorder/recorder-e2e-tentativa-1.mp4` e
`media-verification-tentativa-1.json`. A saída MP4 padrão agora contém a segunda tentativa.

### Execução pelo usuário com `!` no CLI

O usuário executou o script pelo atalho `!` do CLI, que entregou a saída como
`user_shell_command`. Corrigida a orientação anterior do GPT: esse atalho executa comandos
na sessão, e não se pode tratar todo comando enviado pelo usuário como texto sem execução.
Não consultamos o desktop dessa execução; não afirmar que ocorreu no desktop normal só
porque o script se chama `rodar-teste-primeiro-plano.ps1`.

O log `hardware-desktop-normal.log` foi criado. Teste passou (1 teste, 0 falhas), ainda
com primeiro plano `false`. A análise do MP4 atualizado confirmou 3,027 s, 110.868 bytes,
tom de 440,005 Hz e 90 quadros apresentados escuros idênticos na redução para 64×36.
SHA-256: `1ac633ff0e3e7c2bc2872941f8d8a653cb228788f0bf3f5a072b4cae2ea6c444`.
A validação visual segue pendente. Próxima informação útil: se a janela colorida apareceu
no desktop visível do usuário nessa execução.

O usuário confirmou depois que a janela apareceu por cima das demais e que clicou nela.
Portanto, o diagnóstico do desktop isolado das execuções do GPT não deve ser estendido
à execução com `!`. O `false` inicial é consultado após apenas 2 s de tentativas; sozinho
não revela se o clique ocorreu depois ou se a janela ganhou/perdeu foco ao longo do teste.

Para distinguir esses cenários, o roteiro local foi atualizado para iniciar diretamente
o mesmo executável de teste já compilado e observá-lo via `observe_focus.py` (stdlib Python).
O observador registra em `focus-observations.json` o desktop do observador, o desktop de
entrada, o desktop da janela de teste e mudanças no foco a cada 50 ms durante toda a
execução. Não registra nomes/conteúdo de outros apps nem muda foco, desktop ou entrada.
A sintaxe Python/PowerShell e o caminho do executável foram conferidos; essa nova
execução pelo usuário ainda está pendente. Código do gravador e do teste Rust não mudaram.

Esse teste não valida a detecção de um jogo real, o registro do atalho nem as fontes WASAPI
de jogo/Discord/microfone. Essas partes continuam **não verificadas com hardware**.
Permanecem os limites já documentados da captura DDA, inclusive sobreposições visíveis.

**Veredito: aprovado**, com validação de imagem pendente e REC-1 registrado como menor,
sem prioridade de implementação. O teste confirma encoder, áudio sintético, coleta e
MP4 reproduzível; não confirma captura das cores/movimento nem gravação de jogo real.
