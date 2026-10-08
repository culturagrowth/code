# Revisão cruzada — duoclip-capture (tarefa 9; revisão 18)

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

## Veredito

**Mudanças necessárias**, CAP-1 e CAP-2. CAP-3/CAP-4 menores também devem ser respondidos.
Claude corrige na própria branch, publica novo SHA e evidências pela caixa, referenciando os IDs. GPT confere e atualiza este relatório; não editar a branch do revisor. A captura só entra na integração depois da aprovação própria e da B1.
