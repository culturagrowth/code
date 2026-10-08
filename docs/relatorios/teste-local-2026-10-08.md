# Teste local do DuoClip — 08/10/2026

Primeiro teste em uma máquina Windows real, feito com o Claude Code local seguindo [`docs/PROMPT-CLAUDE-CODE-LOCAL.md`](../PROMPT-CLAUDE-CODE-LOCAL.md).
Branch `claude/sync-gameplay-clip-app-xpgfwx`, a partir do commit `4e9b5f7`.

## 1. Ambiente

| Item | Valor |
|---|---|
| Sistema | Windows 11 Pro, **build 26300.9457** (DisplayVersion 26H2). O registro ainda diz "Windows 10 Pro" em `ProductName`, uma peculiaridade conhecida do Windows 11. |
| CPU | AMD Ryzen 5 5500X3D (6 núcleos / 12 threads) |
| RAM | 32 GB |
| GPU | **NVIDIA GeForce RTX 5060 Ti** 16 GB, driver 32.0.16.1714. O DXGI lista o adaptador **duas vezes** (LUIDs `00014B6E` e `00020C7C`); só o primeiro tem saídas. Há também o "Parsec Virtual Display Adapter" instalado (não aparece no DXGI) e o Microsoft Basic Render Driver. |
| Monitores | 3 monitores, todos na RTX 5060 Ti: **DISPLAY1 2560×1440 a 180 Hz, HDR LIGADO, 10 bits** (principal); DISPLAY2 1080×1920 a 75 Hz, **girado 90°**; DISPLAY3 2560×1080 a 75 Hz |
| HAGS | `HwSchMode=2` → ligado |
| WGC | `GraphicsCaptureSession`, `IsBorderRequired`, `IsCursorCaptureEnabled` e `GraphicsCaptureAccess` **presentes** (esperado no Win11) |
| Ferramentas | git 2.54; Node 24.15.0; ffmpeg/ffprobe 8.1.1 (gyan.dev full, com libx264 e aac); **instalados nesta sessão, com confirmação do usuário:** Visual Studio Build Tools 2022 17.14 (carga C++, Windows SDK 10.0.26100) e rustup 1.29 → **Rust 1.99.0** `stable-x86_64-pc-windows-msvc` |
| PresentMon / Medal | não instalados → item 4e não foi feito |

## 2. Testes automáticos

| Comando | Resultado |
|---|---|
| `cargo fmt --all -- --check` | ✅ passou |
| `cargo clippy --workspace --all-targets -- -D warnings` | ❌ na 1ª rodada (lint novo do Rust 1.99, ver 2.1) → ✅ depois da correção |
| `cargo test --workspace` (MSVC nativo) | ✅ **367 passaram, 0 falharam, 4 ignorados** antes do crate de diagnóstico. ✅ **374 / 0 / 4** com o `duoclip-smoke` (+7 testes). Os testes do mux com ffmpeg rodaram (ffmpeg presente). |
| `cargo check --workspace --all-targets --target x86_64-pc-windows-gnu` | ✅ passou (o prompt pede esse check só fora do Windows, mas o CLAUDE.md pede sempre) |
| Testes ignorados do `duoclip-audio` (`-- --ignored`, com autorização para gravar) | ❌ **`process_loopback_of_self_starts_and_stops` derrubava o processo com `STATUS_HEAP_CORRUPTION` (0xC0000374)** → ✅ 3/3 passando, 3 vezes seguidas, depois da correção (ver 2.2) |
| Teste ignorado do `duoclip-clock` (`time.cloudflare.com`, UDP 123) | ✅ passou |
| `cd worker && npm ci && npm run typecheck && npm test` | ✅ `tsc` ok; **312 testes passando em 15 arquivos** |

Contagem por binário de teste (MSVC): audio 32 (+3 ignorados), buffer 0+11+14+8+2+7+4, clock 61 (+1 ignorado) +20+11, crypto 6+52, encode 0,
gamesdb 50, mux 22+6 (ffmpeg)+9, proto 0+44, doc-tests 8 e smoke 7.

### 2.1 Compilação nativa MSVC

**Primeira compilação nativa com MSVC: nenhum erro de compilação.** O único problema foi um lint novo do clippy no Rust 1.99, que o projeto (feito no 1.97) não tinha:

```
error: using `chunks_exact` with a constant chunk size
  --> crates\duoclip-audio\src\lib.rs:98:10
   |
98 |         .chunks_exact(size_of::<f32>())
   |          ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ help: consider using `as_chunks` instead
   = note: `-D clippy::chunks-exact-to-as-chunks` implied by `-D warnings`
```

**Correção:** `samples_from_ne_bytes` passou a usar `bytes.as_chunks::<4>()` (mesmo comportamento: a sobra parcial é ignorada, sem exigir alinhamento).

### 2.2 Bug real: double free na ativação do process loopback

O código `wasapi/` nunca tinha sido executado. Na primeira execução, **todo início de process loopback derrubava o processo** com `0xC0000374 STATUS_HEAP_CORRUPTION`.
O endpoint loopback e o microfone funcionavam.

Como o bug foi isolado (instrumentação temporária, já removida):

1. A ativação funcionava (`GetActivateResult` = `S_OK`, `cast::<IAudioClient>` ok).
2. O crash acontecia no `Drop` de `ActivationParams`, ao liberar o `Box<AUDIOCLIENT_ACTIVATION_PARAMS>`, que é o blob do PROPVARIANT.
3. `HeapValidate` do heap inteiro dava OK antes do free, mas uma nova alocação de 12 bytes **reutilizava o endereço do blob** → o blob já tinha sido liberado por outra pessoa.
4. Até o `drop(operation)` o blob continuava vivo. Ele era liberado dentro do nosso próprio `Drop`, no `drop(Box::from_raw(self.prop))`.
5. Causa: o crate `windows` 0.62 **implementa `Drop for PROPVARIANT`** em `src/extensions/Win32/System/StructuredStorage.rs`, chamando `PropVariantClear`. Para `VT_BLOB`, isso faz `CoTaskMemFree(pBlobData)`, que liberou o nosso `Box` (mesmo heap do processo). Logo depois, o código liberava o mesmo `Box` → double free.
   O comentário no código dizia o contrário ("PROPVARIANT has no Drop impl").

**Correção** (`crates/duoclip-audio/src/wasapi/activate.rs`): o `Box<PROPVARIANT>` passou a ser liberado como `Box<ManuallyDrop<PROPVARIANT>>`
(`repr(transparent)`, mesmo layout), sem rodar o `PropVariantClear`. Ficou documentado no código e nas "Implementation notes" do `SPEC.md` do áudio.

> Não é uma diferença entre versões do Windows: o `Drop` vem do crate Rust. Então o bug existiria em **qualquer** Windows, inclusive no 19045.

## 3. Probes no Windows (`crates/duoclip-smoke`)

Crate novo com 4 binários. Ele compila em qualquer sistema (fora do Windows, cada binário imprime "somente Windows") e grava as saídas em `test-output/`, que está no `.gitignore`.
Detalhes no [`SPEC.md`](../../crates/duoclip-smoke/SPEC.md).

### a) `sysinfo`

Os números estão na seção 1. Pontos relevantes para o DuoClip:

- O monitor principal está com **HDR ligado**. O Desktop Duplication vai precisar do `DuplicateOutput1` com conversão, ou de captura FP16 + tone-map (já previsto na arquitetura). **Não testado:** a captura de tela foi pulada a pedido do usuário.
- Um monitor está **girado 90°**. O recorte do DDA precisa tratar `DXGI_OUTDUPL_DESC.Rotation` (o `capture_probe` ainda não trata).
- O adaptador aparece duplicado no DXGI. Escolha a GPU pela saída/monitor (`IDXGIOutput::GetDesc().Monitor`), não pelo nome.

### b) `audio_probe`

- Raízes do Discord: `Discord.exe` (pid 7604) e `DiscordCanary.exe` (pid 37020), com 6 processos em cada árvore. O Canary ficou de fora a pedido do usuário (é a conta de trabalho), com a nova opção `--skip-exe`.
- Captura: 3 rodadas de 10 s do Discord (process loopback, **1 tentativa por rodada**, sem retry) + microfone. Depois da correção da seção 2.2:

| Rodada | Fonte | Ativação | Pacotes | Silêncio (conteúdo) | Descontinuidades | Extrapolados | Saltos > 2 ms | Drift vs QPC | Pico |
|---|---|---|---|---|---|---|---|---|---|
| 1 | Discord | OK, 3,7 ms | 1003 | 100% | 0 | 0 | 0 | 0,0 ppm | silêncio digital |
| 1 | Mic | OK, 34,7 ms | 1001 | 0% | 1 (pacote 0) | 0 | 0 | −19,2 ppm | −13,3 dBFS |
| 2 | Discord | OK, 1,1 ms | 1001 | 61,8% | 0 | 0 | **1 (8,68 ms)** | 0,0 ppm | −0,4 dBFS |
| 2 | Mic | OK, 22,5 ms | 1001 | 0% | 1 (pacote 0) | 0 | 0 | −15,3 ppm | −44,1 dBFS |
| 3 | Discord | OK, 1,0 ms | 1000 | 100% | 0 | 0 | **1 (8,06 ms)** | 0,0 ppm | silêncio digital |
| 3 | Mic | OK, 21,3 ms | 1001 | 0% | 1 (pacote 0) | 0 | 0 | −16,4 ppm | −61,7 dBFS |

Conclusões:

- **Process loopback do Discord funcionou 3/3 no Windows 11 26300**, sem falhas intermitentes. O pedaço com voz/som da rodada 2 foi capturado, chegando a −0,4 dBFS.
  O Windows 10 19045 continua **sem teste** (é lá que há as falhas conhecidas).
- O process loopback entrega pacotes de 10 ms mesmo em silêncio, **sem a flag `SILENT`** (0% pela flag, até 100% pelo conteúdo). Para detectar silêncio, é preciso olhar as amostras.
- **Os timestamps do process loopback são sintéticos:** cada pacote fica exatamente 100.000 × 100 ns depois do anterior, então "não há drift" por construção.
  Em 2 de 3 rodadas houve **um salto de ~8–9 ms sem nenhuma flag**, no meio de um trecho de silêncio. Esse salto fica abaixo do limiar de 20 ms do `TimestampTracker`, então **não é marcado como descontinuidade**.
  A primeira análise mostrou isso como "+1200 ppm de drift". O probe foi corrigido para contar saltos acima de 2 ms e calcular o drift só em trechos sem salto.
- O microfone tem QPC real (jitter de ±0,08 ms) e drift verdadeiro de −15 a −19 ppm. A flag de descontinuidade aparece só no 1º pacote (início do stream).
- As ativações levaram de 1 a 4 ms (Discord) e de 21 a 35 ms (mic).

### c) `encoder_probe`

| Tipo | Hardware | Software |
|---|---|---|
| H.264 | **NVIDIA H.264 Encoder MFT** (VEN_10DE), ativação OK | H264 Encoder MFT (Microsoft), OK |
| HEVC | NVIDIA HEVC Encoder MFT, OK | HEVCVideoExtensionEncoder, OK |
| AV1 | NVIDIA AV1 Encoder MFT, OK | nenhum |
| AAC | — | Microsoft AAC Audio Encoder MFT, OK |

- O atributo `MFT_ENUM_ADAPTER_LUID` não veio preenchido nos MFTs da NVIDIA. Para saber o adaptador, use o vendor ID ou crie o MFT com o device D3D.
- **Teste de codificação com `duoclip-encode` + `duoclip-mux`: não feito**, porque o `duoclip-encode` ainda é só um esqueleto.
- Teste complementar com o ffmpeg (quadros sintéticos `testsrc2` 1080p60, 3 s, GOP 60, sem B-frames, 40 Mbps):
  - `h264_nvenc` → ffprobe: h264 1920×1080, **180 quadros, 3,000 s, 60 fps** ✅
  - `h264_mf -hw_encoding 1` (o MFT de hardware da NVIDIA) **falhou com a entrada yuv420p** ("format negotiation failed") e **funcionou com `-pix_fmt nv12`** (180 quadros, 3,000 s) ✅.
    **Para o `duoclip-encode`:** entregue NV12 ao MFT de hardware (conversão BGRA→NV12 na GPU, pelo Video Processor).

### d) `capture_probe`

**Não executado:** o usuário preferiu pular a captura de tela nesta rodada. O binário compila e está pronto
(`capture_probe --list-windows`, depois `capture_probe --window "<título>"`). Só a listagem de janelas, que não captura nada, foi testada.

### e) PresentMon / Medal

Não instalados → não feito.

## 4. Problemas encontrados

1. **Crítico, corrigido:** double free na ativação do process loopback (seção 2.2). Todo process loopback derrubava o app.
2. **Médio, aberto:** o process loopback dá saltos de ~8–9 ms no timestamp sem flag, abaixo do limiar de 20 ms do `TimestampTracker`. Na sincronia com o vídeo, isso vira um erro de até ~9 ms que ninguém detecta.
3. **Baixo, corrigido:** lint `chunks_exact_to_as_chunks` do clippy no Rust 1.99.
4. **Informativo:** o process loopback não usa a flag `SILENT`; o MFT H.264 de hardware da NVIDIA exige NV12; o LUID não vem nos MFTs; o adaptador aparece duplicado no DXGI; há HDR e monitor girado na máquina de teste.

## 5. Correções feitas

- `crates/duoclip-audio/src/lib.rs`: `as_chunks` no lugar de `chunks_exact` (clippy).
- `crates/duoclip-audio/src/wasapi/activate.rs`: o PROPVARIANT é liberado como `ManuallyDrop` (double free).
- `crates/duoclip-audio/SPEC.md`: "Implementation notes" com os dois achados.
- Novo `crates/duoclip-smoke` (4 binários, 7 testes portáveis, `SPEC.md`).

Depois das correções, **todas** as checagens do item 3 foram rodadas de novo: fmt ✅, clippy ✅, 374 testes ✅, check gnu ✅, worker 312 ✅ (sem mudanças).

## 6. Recomendações

1. **Rodar o `capture_probe`** nesta máquina (monitor principal HDR a 180 Hz, de preferência com um jogo aberto) e, se possível, instalar o PresentMon para o item 4e.
2. **Rodar `audio_probe --capture` num Windows 10 19045** de um amigo: é lá que estão as falhas intermitentes conhecidas.
3. `TimestampTracker` nas faixas de process loopback: usar um limiar de salto de ~2 ms, ou tratar o QPC delas como sintético e reancorar periodicamente pelo QPC de chegada.
4. `duoclip-encode`: entregar NV12 ao MFT de hardware e escolher a GPU pela saída do monitor capturado.
5. `capture` (Fase B2): tratar `Rotation` e HDR (`DuplicateOutput1` com FP16 + tone-map) desde o começo, porque a máquina de teste já tem os dois.
6. Fixar a versão do Rust (`rust-toolchain.toml`) para evitar lints novos quebrando o `-D warnings` em máquinas diferentes. **Não feito**, porque é uma decisão do projeto.
