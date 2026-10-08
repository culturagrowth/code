# Revisão cruzada — duoclip-capture (tarefa 9; revisão 18)

**Veredito atual: aprovado com ressalvas**, para `9b62edb4db369a8981d5cebf1611c87e739d8661`, após a conferência da resposta `2026-10-08-claude-019-ajustes-duoclip-capture`. A segunda rodada está ao final deste relatório. O histórico abaixo corresponde à primeira entrega e não representa o veredito atual.

Revisor: GPT, 08/10/2026. Pedido: `2026-10-08-claude-015-pedido-revisao-duoclip-capture`.
Código do Claude: `a6fbc2fcc566c66b7f27be5ed61ff953bbcf1745`, branch `claude/duoclip-capture`.
Diff exclusivo: base `3056288aac6542a64a95cc53f2650c531f378275` até a entrega.
Revisão em worktree próprio `worktrees/gpt-revisao-duoclip-capture`, branch `gpt/revisao-duoclip-capture`; nenhuma implementação alterada.
Contrato: `crates/duoclip-capture/SPEC.md` e AGENTS.md canônico da integração.

## Validação independente

- `cargo fmt --all -- --check`: passou.
- `cargo clippy --workspace --all-targets -- -D warnings`: passou.
- `cargo test --workspace`: 445 passaram, 0 falharam, 18 ignorados; captura com 31 testes unitários + 1 doctest.
- `cargo check --workspace --all-targets --target x86_64-pc-windows-gnu`: passou.
- Probe adicional sem gravação: reproduziu a decisão Game durante perda de foco, aceitação de PID divergente e inconsistência de geometria no caminho defensivo de textura menor.

Logs e probe ignorados neste worktree: `test-output/review-capture/{fmt,clippy,tests,gnu,probe}.log` e `test-output/review-capture/probe/{Cargo.toml,src/main.rs}`. O probe consulta somente handle/estado da janela desktop, não pixels nem títulos de janelas.

Os seis testes `capture_hw` não foram executados pelo GPT: os que duplicam a tela exigem confirmação explícita, conforme o pedido e AGENTS.md. As evidências reais publicadas pelo Claude permanecem atribuídas a ele. Não foi solicitado bloquear a revisão aguardando essa gravação. Windows 10 real, ACCESS_LOST/UAC, troca entre adaptadores, rotação/HDR e MP4 de captura: **não verificados em execução independente**. Linux real: não verificado. Os oito testes GPU sintéticos de encode foram executados e passaram na revisão B1, separadamente.

## CAP-1 — importante: debounce permite copiar pixels de outra janela

Locais: `crates/duoclip-capture/src/focus.rs:102`, `src/win/dda.rs:561` e `src/types.rs` (opções de foco).

Após o jogo ser observado em foreground, o tracker continua retornando Game por até 250 ms mesmo quando foreground=false. O backend trata essa decisão como permissão para copiar a superfície DDA no retângulo do jogo. DDA fornece a imagem do desktop composto; o retângulo não isola o conteúdo original da janela. Portanto, se o usuário alternar para outra janela que cubra o jogo sem minimizá-lo, essa outra janela pode aparecer nos quadros emitidos como Game durante a graça. `require_foreground=false` permite a mesma situação por tempo indefinido.

Reprodução da decisão com dados sintéticos: WindowState existente, não minimizada, foreground=true em t=0; mesmo estado com foreground=false em t=100 → Game. Confirmado no probe; não houve gravação de conteúdo privado. A consequência sobre os pixels é inferida do caminho CopySubresourceRegion e da API de [Desktop Duplication](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/desktop-dup-api), que captura o desktop. O próprio cabeçalho de `tests/capture_hw.rs` reconhece que sobreposição de outra janela termina no recorte.

O SPEC explicitamente pede debounce, mas também descreve captura apenas do jogo/placeholders sem desktop. Essas duas expectativas precisam ser reconciliadas: debounce de estado de UI não pode autorizar copiar uma região potencialmente coberta.

Sugestão: interromper novas cópias assim que a condição segura se perder, repetindo o último quadro comprovadamente seguro ou emitindo placeholder; manter debounce separado dessa permissão. Para captura de janela em background, requerer verificação de oclusão/região segura ou um backend que isole a janela. Não considerar a simples visibilidade como prova de ausência de sobreposição. Testar foco perdido sem minimize, janela cobrindo o alvo e retomada, com a decisão de copiar exercitada em dados sintéticos; captura real adicional somente após autorização.

## CAP-2 — importante: HWND válido não comprova a identidade do alvo

Locais: `crates/duoclip-capture/src/win/window.rs:75`, `src/win/window.rs:122` e `src/win/dda.rs:116`.

start verifica que o HWND existe; window_state também verifica apenas IsWindow antes de coletar o estado. O PID informado serve como alternativa para reconhecer foreground, sem validar o dono do HWND alvo. Se a janela do jogo morrer e seu handle for reciclado antes do próximo polling, uma janela de outro processo pode ser aceita e capturada como o jogo; quando essa nova janela é foreground, a comparação direta fg==h já autoriza Game.

Confirmado sem captura: window_state de um HWND existente com PID conhecido divergente e require_foreground=false retorna exists=true e foreground=true. Não foi reproduzida uma reciclagem real; o risco de reciclagem é documentado em [IsWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-iswindow), e o código não possui a comparação necessária para bloquear o caso entre processos.

Sugestão: validar HWND/PID no início e a cada amostragem; quando PID=0, fixar a identidade inicial em vez de manter somente um handle. Divergência ou falha da consulta deve encerrar como WindowNotFound/Gone. Revalidar o alvo antes de entregar pixels e definir o tratamento de destruição/recriação da janela. Testar PID divergente, falha de consulta e troca de identidade, sem duplicar a tela.

## CAP-3 — menor: desktop do CropPlan não acompanha o clamp rotacionado

Local: `crates/duoclip-capture/src/geom.rs:259`.

No caminho defensivo em que o tamanho da textura difere do monitor, o clamp muda src e o ponto inicial correspondente no desktop para rotações 180/90/270. O campo desktop, porém, conserva vis.left/top.

Reprodução portátil: monitor e window=(0,0,100,100), Rotate180, textura 50×100. O plano retorna desktop=(0,0,50,100); source_texel(0,0)=(49,99), mas desktop_to_texture(desktop.left,desktop.top,monitor,Rotate180)=(99,99). O ponto realmente mostrado é desktop=(50,0). A saída continua dentro da textura; o defeito é a correspondência contratada e a preservação do canto inicial. Texturas normais dos testes não falharam.

Sugestão: calcular desktop pela transformação inversa do src efetivamente clampado e aplicar o trim de forma consistente, ou rejeitar dimensões incompatíveis. Expandir a propriedade de correspondência de pixels dos testes para texturas incompatíveis e todas as rotações; o teste atual de mismatch cobre só Identity.

## CAP-4 — menor: notas de implementação contradizem a entrega

Local: `crates/duoclip-capture/SPEC.md:154`.

O SPEC afirma que os testes de hardware nunca foram rodados pelo autor; a mensagem Claude015 apresenta execução e resultados reais. Atualizar a evidência histórica com atribuição/data e apontar para o relatório, sem tratar execução do autor como verificação do revisor. Os testes rotated_monitor_report/hdr_monitor_report também só reportam divergências, por contrato: resultado verde sozinho não comprova rotação/cores; preservar as linhas de evidência.

Também ajustar a afirmação de que DDA sempre exclui o cursor (`SPEC.md:126`): a documentação de [Desktop Duplication](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/desktop-dup-api) descreve casos em que o ponteiro já está na imagem, além de casos em que é separado. A implementação não compõe um cursor adicional, mas isso não garante sua ausência em todo driver.

## Demais pontos conferidos

Geometria nominal e shader seguem as fórmulas inversas do [sample oficial da Microsoft](https://raw.githubusercontent.com/microsoft/Windows-classic-samples/main/Samples/DXGIDesktopDuplication/cpp/DisplayManager.cpp). Conferidos negativos, bordas exclusivas, rotação, tamanhos pares e aritmética i64/i128. Testes portáveis de valores extremos passaram.

Conferidos seleção de output por HMONITOR no adaptador do dispositivo, formatos HDR e fallback, SDR white, RAII de ReleaseFrame em saídas de erro, submissão antes de liberação, ring emprestado até o retorno do sink, locks do contexto, placeholders integralmente limpos, limites de retry, sinalização inicial, stop/join e stubs Unsupported. A posse documentada do ring de captura pressupõe trabalho GPU submetido no callback; o achado B1-E1 do ring NV12 do encoder continua separado e bloqueia a integração da base.

## Veredito da primeira rodada (histórico)

**Mudanças necessárias**, CAP-1 e CAP-2. CAP-3/CAP-4 menores também devem ser respondidos.
Claude corrige na própria branch, publica novo SHA e evidências pela caixa, referenciando os IDs. GPT confere e atualiza este relatório; não editar a branch do revisor. A captura só entra na integração depois da aprovação própria e da B1.

## Segunda rodada — correções conferidas em 08/10/2026

Código examinado: `9b62edb4db369a8981d5cebf1611c87e739d8661`, branch `claude/duoclip-capture`, já sobre B1 corrigida `2f3a22b0a2f88c5f1f3add6c314e8f7f204f050a`. A primeira implementação virou `6ff95c1` no rebase do autor; o diff das correções é `6ff95c1..9b62edb`. A branch própria do revisor foi rebaseada sobre a entrega nova, preservando o relatório histórico. Nenhuma implementação foi alterada pelo GPT.

### Resposta aos achados

- **CAP-1: corrigido no cenário reportado de perda de foco.** `FocusTracker::update` (`src/focus.rs:125`) distingue `CopyGame`, `HoldLast`, `Placeholder` e `Gone`. Durante a graça, `HoldLast` não autoriza copiar nem entrega quadro; o pacer pode repetir o último quadro. O backend trata essa decisão separadamente (`src/win/dda.rs:668`). `SafeStreak` também rejeita imagens anteriores ao começo da sequência permitida e aplica a margem de um período de refresh. Os testes cobrem perda de foco sem minimizar, retomada, debounce e timestamps. A margem é uma hipótese defensiva, explicitamente documentada, sem garantia da Microsoft. Permanecem os limites abaixo.
- **CAP-2: corrigido para dono divergente ou consulta inválida.** `TargetIdentity::pin` valida o PID informado e fixa PID/thread quando PID=0; `window_state` confere o dono antes/depois das consultas e o backend revalida antes da entrega. `start` rejeita a identidade inválida antes de criar a duplicação. GPT executou o teste específico com janela invisível message-only, PID errado, HWND inválido e janela destruída: passou, sem capturar pixels. Isso comprova a rejeição do cenário reportado, sem reproduzir reciclagem real de HWND.
- **CAP-3: corrigido.** `CropPlan.desktop` é calculado pela inversa do recorte efetivo (`src/geom.rs:274`). O caso Rotate180/textura 50×100 agora corresponde a desktop `(50,0,100,100)`. A propriedade de correspondência cobre texturas menores, maiores, ímpares e trocadas nas quatro rotações (`src/geom.rs:689`).
- **CAP-4: corrigido.** SPEC registra data e autoria dos testes reais, distingue os testes que somente reportam e reconhece que um cursor já incorporado pelo driver pode aparecer. Evidência do Claude continua atribuída ao Claude.

### Validação independente da segunda rodada

- `cargo fmt --all -- --check`: passou.
- `cargo clippy --workspace --all-targets -- -D warnings`: passou.
- `cargo test --workspace`: **476 passaram, 0 falharam, 24 ignorados**; captura com 41 testes unitários e 1 doctest.
- `cargo check --workspace --all-targets --target x86_64-pc-windows-gnu`: passou.
- `cargo test -p duoclip-capture --test capture_hw -- --ignored --exact identity_mismatch_rejected_without_capture --nocapture --test-threads=1`: **1 passou**, 7 filtrados, 0,26 s; `WindowNotFound` para PID errado, nenhum quadro entregue e estatísticas zeradas. Esse teste apenas cria uma janela message-only e um dispositivo D3D11; não duplica a tela.

Logs locais ignorados: `worktrees/gpt-revisao-duoclip-capture/test-output/review-capture-r2/{fmt,clippy,tests,gnu,identity}.log`. A compilação reutilizou o target do worktree próprio de revisão B1. Os testes sintéticos de GPU e pool foram conferidos separadamente em B1 (11 + 1 passaram); ver `fase-b1.md`.

GPT não executou os testes que gravam a tela. Na mensagem Claude019 e no SPEC, o autor relata teste estrito de foco perdido sem minimizar (0 quadros Game depois da troca observada), restauração, identidade, rotação, HDR e MP4. São resultados do autor, não execução independente do GPT. Windows 10 real, captura/cores/rotação reais, mudança de adaptador e UAC continuam não verificados pelo revisor.

### Ressalvas e alcance da aprovação

1. **DDA ainda recorta o desktop composto.** Foreground não prova ausência de sobreposição: janelas sempre no topo, notificações e outras janelas aceitas como foreground do jogo podem aparecer no recorte. Perder e recuperar foco inteiramente entre duas amostras também pode escapar do polling. O SPEC declara essas limitações; a margem de `SafeStreak` reduz a corrida, sem eliminá-la. Esta aprovação do crate com as limitações documentadas **não comprova o requisito de capturar exclusivamente pixels do jogo**. Para prometer esse isolamento, será necessário resolver oclusão ou usar um backend que isole a janela, respeitando as regras de borda/Windows 10 do projeto.
2. **Identidade é verificação de dono, não de geração do HWND.** PID/thread fixados barram mudança de dono; uma eventual reutilização do mesmo handle pelo mesmo PID/thread não é distinguida pelo comparador atual. Não foi reproduzida essa reutilização; não tratar a checagem como garantia absoluta contra qualquer reciclagem.
3. `CaptureOptions::test_only_copy_without_foreground` continua público apesar de `doc(hidden)`, mas é false por padrão. Ativá-lo libera cópia sem foreground e invalida a proteção de CAP-1; deve permanecer restrito a testes expressamente autorizados e não entrar nas opções do produto.

**Veredito: aprovado com ressalvas.** Os cenários dos achados CAP-1 a CAP-4 foram respondidos e conferidos no alcance descrito. Não há novo bloqueio de integração do crate nesta rodada; os limites de privacidade devem acompanhar a entrega, sem anunciar isolamento absoluto. B1 foi aprovada com ressalva menor independente B1-E4 (validação da textura no encoder SoftwareOnly), registrada no relatório B1 e em GPT018.

Claude deve integrar o SHA examinado e os dois commits de documentação da branch do revisor (relatório histórico + atualização), ou merge equivalente, preservando autoria. Se alterar implementação após esta entrega, publicar o novo SHA para conferência. Responder pela caixa canônica; não editar o worktree do GPT.
