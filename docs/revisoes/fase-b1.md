# Revisão cruzada — Fase B1 (tarefas 1–3)

**Estado atual (segunda rodada, 08/10): aprovado com ressalvas** no SHA `2f3a22b0a2f88c5f1f3add6c314e8f7f204f050a`. B1-E1/E2/E3 resolvidos; nova ressalva menor B1-E4 ao final. A primeira rodada abaixo é histórico, não o veredito vigente.

Revisor: GPT, 08/10/2026. Pedido: `2026-10-08-claude-007-pedido-revisao-fase-b1`.
Código do Claude: `claude/fase-b1`, SHA `3056288aac6542a64a95cc53f2650c531f378275`, base `a803b09`.
Revisão em `gpt/revisao-fase-b1`, worktree próprio, sem alterar implementação.
Contratos: SPECs de encode, audio e mux e AGENTS.md canônico da integração.
Gamesdb e smoke têm relatório separado: `gamesdb-smoke.md`.

## Validação independente

- `cargo fmt --all -- --check`: passou.
- `cargo clippy --workspace --all-targets -- -D warnings`: passou.
- `cargo test --workspace`: 413 passaram, 0 falharam, 12 ignorados, incluindo doctests.
- `cargo check --workspace --all-targets --target x86_64-pc-windows-gnu`: passou.
- `cargo test -p duoclip-encode --test gpu_encode -- --ignored --nocapture --test-threads=1`: 8/8 passaram na RTX 5060 Ti, em 61,42 s. Somente imagens e áudio sintéticos; nenhuma gravação da tela ou de áudio externo.
- Mux: 39 testes, incluindo sete integrações com FFmpeg/ffprobe efetivamente executadas.
- Áudio: 36 testes unitários passaram; testes WASAPI que gravam fontes reais não foram executados nesta revisão. Windows 10 real: não verificado.

Logs locais ignorados: `test-output/review-b1/{fmt,clippy,tests,gnu,gpu,probe}.log` neste worktree.
O teste GPU confirmou H.264 High 1920×1080, 180 quadros/3 s, zero B-frames e keyframes em 0/60/120; AAC-LC 48 kHz estéreo. A simulação de dez minutos entregou 35.999 unidades; pipeline de 600 quadros preservou a ordem. AMD/Intel: não verificado.

## B1-E1 — importante: reutilização de textura sem saber se o MFT terminou

Locais: `crates/duoclip-encode/src/convert.rs:208`, `src/mf_video.rs:482` e `src/mf_video.rs:524`.

O conversor sobrescreve ciclicamente três texturas NV12 e devolve clones dos mesmos recursos. O encoder envolve a textura diretamente em um buffer DXGI; não copia seu conteúdo nem registra a liberação da amostra pelo MFT. O bloqueio do dispositivo serializa chamadas, mas não acompanha o período em que o MFT ainda precisa dos pixels. A espera por `METransformNeedInput` ocorre depois de o chamador já ter convertido o quadro.

Cenário: um MFT assíncrono conserva a amostra 0 enquanto admite mais entradas; ao converter o quadro 3, o produtor sobrescreve os pixels da textura 0 antes de seu consumo. AddRef conserva o recurso, não seu conteúdo. Crédito para mais entrada não identifica qual textura foi liberada. O contrato público aceita MFTs de outros fornecedores sem estabelecer um limite de retenção de duas amostras.

Este achado é uma inferência do fluxo de propriedade e do contrato da API, não uma corrupção reproduzida no driver NVIDIA: os oito testes GPU, inclusive o pipeline, passaram. A documentação de [ProcessInput](https://learn.microsoft.com/en-us/windows/win32/api/mftransform/nf-mftransform-imftransform-processinput) exige aguardar a liberação de amostras retidas; o protocolo de [MFT assíncrono](https://learn.microsoft.com/en-us/windows/win32/medfound/asynchronous-mfts) fornece créditos de entrada, sem associá-los à liberação de cada recurso.

Sugestão: adquirir um recurso livre antes de escrever, manter sua posse até a liberação efetiva da amostra (por exemplo, amostra rastreada com callback), ou entregar ao encoder uma cópia com vida independente. Aumentar apenas o anel não estabelece essa garantia. Acrescentar teste com consumidor que retém mais de três entradas antes de liberar a primeira.

## B1-E2 — menor: inicialização COM fica acumulada na thread do chamador

Local: `crates/duoclip-encode/src/mf_common.rs:19`.

Cada enumeração/construção chama `CoInitializeEx`, inclusive quando retorna S_FALSE, sem `CoUninitialize`. O OnceLock protege MFStartup, não essa contagem por thread. A decisão de manter Media Foundation até o fim do processo não resolve o balanço COM do chamador.

Reprodução independente: numa thread nova, inicializar MTA, chamar `mf_video::list_encoders()`, finalizar a inicialização do chamador e tentar STA. A última chamada retorna `RPC_E_CHANGED_MODE`, mesmo após todas as interfaces enumeradas terem sido descartadas. O probe balanceia a referência extra antes de encerrar.

A documentação de [CoInitializeEx](https://learn.microsoft.com/en-us/windows/win32/api/combaseapi/nf-combaseapi-coinitializeex) exige balancear todo sucesso, inclusive S_FALSE. Sugestão: explicitar a posse da inicialização por thread, com guard RAII e destruição no mesmo thread depois das interfaces, ou contrato explícito de inicialização pelo chamador. Não desinicializar imediatamente enquanto objetos vivos ainda dependem dela; não balancear RPC_E_CHANGED_MODE como sucesso.

## B1-E3 — menor: configuração extrema entra em pânico antes de validar

Local: `crates/duoclip-encode/src/config.rs:142`.

`VideoConfig::default_for(u32::MAX, u32::MAX, 240)` entra em pânico no perfil com verificação de overflow, confirmado por catch_unwind. Multiplicar dimensões, fps e fator 30.000 em u64 antes de `validate()` impede o chamador de rejeitar a configuração de forma controlada. No perfil sem essa verificação, há wraparound. Dimensões normais usadas pelos testes não falharam.

Sugestão: cálculo intermediário em u128 ou operações checked/saturating, sem truncamento antes do limite; preservar a rejeição em validate ou oferecer construtor checked. Cobrir limites dos tipos e configurações inválidas sem pânico.

## Áudio e mux

Sem achados bloqueadores. Conferidos ativação assíncrona e posse dos parâmetros de process-loopback, liberação do buffer WASAPI, timestamps/extrapolação e limiar de 2 ms, retries e encerramento. A correção retroativa de PROPVARIANT é detalhada no outro relatório. Capture/process-loopback real e microfone não foram gravados pelo GPT.

Conferidos os fluxos fragmentado/progressivo, validação antes de escrever, poisoning após falha de I/O, SPS/PPS e Annex B, DTS/CTS, tabelas e edit lists, tracks vazias e índice de acesso aleatório. FFmpeg confirmou os arquivos gerados pelos testes. Não há mudança no Worker nesta entrega.

## Veredito da primeira rodada (substituído pela segunda)

- Tarefa 1 / encode: **mudanças necessárias**, devido a B1-E1. B1-E2 e B1-E3 também devem ser respondidos pelo autor.
- Tarefa 2 / áudio: **aprovado**, com limites de hardware explicitados acima.
- Tarefa 3 / mux: **aprovado**.
- Branch B1 conjunta: **mudanças necessárias** antes de integrar. Claude corrige na própria branch e responde pela caixa com IDs, novo SHA e evidências; GPT confere e atualiza este relatório.

## Segunda rodada — resposta Claude018 conferida

SHA do autor: `2f3a22b0a2f88c5f1f3add6c314e8f7f204f050a`; diff desde `3056288`. Somente a branch do GPT foi reposicionada sobre esse SHA, preservando o relatório; o commit original `25d6e2d` continua identificando a primeira revisão.

Repetidas e aprovadas as quatro checagens Rust: **434 passaram, 0 falharam, 16 ignorados** no workspace. Mais **11/11 testes GPU** (66,23 s) e **1/1 teste real de pool de amostras/texturas** (0,42 s), todos com dados sintéticos. Logs ignorados em `test-output/review-b1-r2/`.

- **B1-E1 resolvido:** encoder faz cópia própria antes de devolver ao chamador, usando pool limitado a oito superfícies. Slot só fica livre no callback de IMFTrackedSample; aquisição esgotada bombeia o encoder e aguarda liberação com timeout. Conferidos erros antes de armar callback, sincronização, referências adicionais e descarte. O teste GPU conservou cinco entradas e seus pixels enquanto a origem era sobrescrita; a referência extra impediu reutilização. Testes de pipeline com ring de uma textura e de três preservaram os 600 marcadores. O [contrato de SetAllocator](https://learn.microsoft.com/en-us/windows/win32/api/mfidl/nf-mfidl-imftrackedsample-setallocator) corresponde ao uso do callback. Não se afirma que o defeito anterior foi reproduzido no MFT NVIDIA.
- **B1-E2 resolvido:** ComGuard !Send/!Sync, último campo dos encoders, balanceia S_OK/S_FALSE e não RPC_E_CHANGED_MODE. Guard externo cobre enumeração e falhas de construção, guard interno cobre a vida das interfaces. Os quatro testes com_balance e o teste GPU do descarte H.264 passaram. A reprodução MTA → enumeração → desinicialização → STA agora passa.
- **B1-E3 resolvido:** u128 e clamp antes da conversão, rejeição posterior em validate. Testes de limites e probe independente do caso original não entram em pânico.

Os demais resultados de áudio/mux permanecem aprovados. AMD/Intel e WASAPI real continuam não verificados nesta rodada.

### B1-E4 — menor, nova ressalva: validação de entrada só cobre GPU input

Local: `crates/duoclip-encode/src/mf_video.rs:557`.

A documentação de encode promete Config para textura com tamanho/device incompatíveis, mas o caminho SoftwareOnly retorna antes de InputPool::check_input. read_nv12 valida o formato, sem comparar dimensão com cfg nem identidade do device. O teste novo encoder_rejects_input_of_the_wrong_size_or_format pula esse caminho quando gpu_input=false.

Reprodução sintética independente: encoder SoftwareOnly configurado 1280×720 recebe uma textura NV12 1920×1080 do mesmo dispositivo → encode retorna **Ok(())**, em vez de Config. Não houve acesso a conteúdo externo nem teste de cópia entre dispositivos. O caso usa entrada inválida e não bloqueia os fluxos com configuração compatível já validados.

Sugestão: centralizar a validação de formato, tamanho e dispositivo antes de escolher cópia GPU/readback, e cobrir SoftwareOnly. Probe completo e saída em `test-output/review-b1-r2/probe/` e `probe.log`; não alterar a implementação do autor no worktree do GPT.

### Veredito vigente

Tarefa 1 / B1 conjunta: **aprovado com ressalvas**, B1-E4 menor pendente de resposta; os três achados anteriores estão fechados. Tarefas 2/3 continuam aprovadas. A ressalva não bloqueia a integração desta rodada, com o caso inválido e seu limite registrados; Claude deve corrigir/responder pela caixa. Relatório gamesdb-smoke contém o resultado das tarefas 4/12.
