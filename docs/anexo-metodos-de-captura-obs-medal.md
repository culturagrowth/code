# Anexo — Que métodos de captura o OBS, o Medal e os outros usam (evidências)

> Anexo do documento [pesquisa-app-clipes-sincronizados.md](pesquisa-app-clipes-sincronizados.md), seção 4.
> Pesquisa feita em 07–08/10/2026, com verificação adversarial, ou seja, uma segunda rodada que tentou refutar cada afirmação.
>
> **Legenda de confiança:**
> - 🟢 **primária**: código-fonte, documentação oficial ou declaração do próprio fabricante;
> - 🟡 **secundária**: logs ou análises de terceiros, fóruns, blogs;
> - ⚪ **inferência**: nossa conclusão a partir das evidências.

---

## 1. OBS Studio: conferido no código-fonte

Lemos o código do OBS (`obsproject/obs-studio`, commit `c5bcbca`, de 05/10/2026). A segunda rodada de verificação conferiu de novo as linhas principais.

### 1.1 Game Capture ("Captura de jogo") é injeção de DLL 🟢

- O OBS **injeta `graphics-hook64.dll` dentro do processo do jogo**.
  - **Modo padrão** ("Usar hook de compatibilidade com anti-cheat", ligado por padrão): o `inject-helper` carrega a DLL via `SetWindowsHookEx(WH_GETMESSAGE)` na thread da janela do jogo.
  - **Com a opção desligada:** a injeção é direta, com `OpenProcess(PROCESS_ALL_ACCESS)` + `VirtualAllocEx` + `WriteProcessMemory` + `CreateRemoteThread(LoadLibraryW)`. Os nomes dessas APIs ficam ofuscados no código.
  - Fontes: [`game-capture.c` L833–961](https://github.com/obsproject/obs-studio/blob/c5bcbca63fc32f8341c08e1f921b2b81ec46be00/plugins/win-capture/game-capture.c#L833-L961), [`inject-library.c` L12–134](https://github.com/obsproject/obs-studio/blob/c5bcbca63fc32f8341c08e1f921b2b81ec46be00/shared/obs-inject-library/inject-library.c#L12-L134) e o padrão `true` em [`game-capture.c` L2108–2120](https://github.com/obsproject/obs-studio/blob/c5bcbca63fc32f8341c08e1f921b2b81ec46be00/plugins/win-capture/game-capture.c#L2108-L2120).
- **O que a DLL faz:** intercepta (com Microsoft Detours) as funções de apresentação de frame de cada API gráfica:
  - DXGI: `Present`, `ResizeBuffers`, `Present1`;
  - D3D9: `Present`, `Reset`;
  - D3D12: `ExecuteCommandLists`;
  - OpenGL: `SwapBuffers` / `wglSwapBuffers`.

  No Vulkan, o OBS se registra como **layer implícita** do loader (`VK_LAYER_OBS_HOOK`), e o próprio loader do Vulkan carrega a DLL em todo app Vulkan. Fontes: [`graphics-hook.c`](https://github.com/obsproject/obs-studio/blob/c5bcbca63fc32f8341c08e1f921b2b81ec46be00/plugins/win-capture/graphics-hook/graphics-hook.c#L303-L417), [`dxgi-capture.cpp`](https://github.com/obsproject/obs-studio/blob/c5bcbca63fc32f8341c08e1f921b2b81ec46be00/plugins/win-capture/graphics-hook/dxgi-capture.cpp#L310-L350), [`obs-vulkan64.json`](https://github.com/obsproject/obs-studio/blob/c5bcbca63fc32f8341c08e1f921b2b81ec46be00/plugins/win-capture/graphics-hook/obs-vulkan64.json).
- **Por que é eficiente:** a cada `Present`, a própria GPU do jogo copia o *backbuffer* para uma **textura compartilhada** (`D3D11_RESOURCE_MISC_SHARED`), que o OBS abre pelo *handle*. A imagem nunca passa pela CPU. Existe um modo mais lento por memória compartilhada (CPU), usado no "SLI/Crossfire". A KB do OBS chama o Game Capture de "o método mais eficiente". Fontes: [`d3d11-capture.cpp`](https://github.com/obsproject/obs-studio/blob/c5bcbca63fc32f8341c08e1f921b2b81ec46be00/plugins/win-capture/graphics-hook/d3d11-capture.cpp#L60-L290), [KB Game Capture](https://obsproject.com/kb/game-capture-source).
- **Anti-cheat:** os anti-cheats toleram o hook do OBS por causa do **certificado de assinatura do OBS**.
  - As notas do OBS 31.0 avisam que a troca de certificado "pode afetar a compatibilidade do game capture com algumas soluções anti-cheat" ([release 31.0.0](https://github.com/obsproject/obs-studio/releases/tag/31.0.0), [KB](https://obsproject.com/kb/capture-hook-certificate-update)).
  - O próprio [`compatibility.json`](https://github.com/obsproject/obs-studio/blob/c5bcbca63fc32f8341c08e1f921b2b81ec46be00/plugins/win-capture/data/compatibility.json) diz que o **CS2 pode exigir a opção `-allow_third_party_software`** para o Game Capture funcionar.
  - ⚪ **Conclusão:** um app novo que injetasse do mesmo jeito **não teria essa exceção** e seria bloqueado ou marcado.
- **Sem plano B automático:** o Game Capture não tem fallback para WGC (o `game-capture.c` não tem nenhum código WGC/WinRT). Para jogos que bloqueiam o hook (Destiny 2, Roblox...), o `compatibility.json` manda o usuário **usar Window Capture ou Display Capture**.

### 1.2 Window Capture ("Captura de janela") 🟢

- Tem três modos: **Automático**, **"Modern (WGC)"** (Windows.Graphics.Capture) e **"Legacy (BitBlt)"**.
- **O Automático usa BitBlt por padrão.** Ele só escolhe WGC quando a classe da janela contém "Chrome" ou "Mozilla", ou está numa lista fixa: UWP/WinUI, Office, `SDL_app`, `WINDOWSCLIENT` (Roblox), WebView2 e Gaming Services. Fonte: [`window-capture.c` L113–169](https://github.com/obsproject/obs-studio/blob/c5bcbca63fc32f8341c08e1f921b2b81ec46be00/plugins/win-capture/window-capture.c#L113-L169).
- **Por que o OBS preferiu BitBlt em 2020** ([commit 572ce731](https://github.com/obsproject/obs-studio/commit/572ce731)): a borda amarela, a falta de controle do cursor e a troca forçada para cursor por software. Hoje o código já remove a borda (`IsBorderRequired(false)`) e controla o cursor (`IsCursorCaptureEnabled`).
- **Como o OBS implementa o WGC** ([`winrt-capture.cpp`](https://github.com/obsproject/obs-studio/blob/c5bcbca63fc32f8341c08e1f921b2b81ec46be00/libobs-winrt/winrt-capture.cpp#L160-L330)):
  - *frame pool* com 2 buffers, criado com `Create` (não `CreateFreeThreaded`), com callbacks na thread gráfica do OBS;
  - **uma cópia extra (`CopyResource`) por frame**. O próprio comentário no código diz: *"if they gave an SRV, we could avoid this copy"*.
- **Medição do OBS** ([PR #2208](https://github.com/obsproject/obs-studio/pull/2208)): o WGC custou "um pouco mais de CPU que o BitBlt (200–800 µs contra 300–500 µs por frame) e um pouco menos de GPU".

### 1.3 Display Capture ("Captura de tela") 🟢

- Usa **DXGI Desktop Duplication** por padrão: `AcquireNextFrame(0)` seguido de `CopyResource`.
- O Automático troca para WGC em dois casos: quando o DXGI não enxerga o monitor (ligado a outra GPU) ou em **notebook híbrido** (tem bateria e 2+ adaptadores). Fontes: [`duplicator-monitor-capture.c`](https://github.com/obsproject/obs-studio/blob/c5bcbca63fc32f8341c08e1f921b2b81ec46be00/plugins/win-capture/duplicator-monitor-capture.c#L250-L301), [`d3d11-duplicator.cpp`](https://github.com/obsproject/obs-studio/blob/c5bcbca63fc32f8341c08e1f921b2b81ec46be00/libobs-d3d11/d3d11-duplicator.cpp#L20-L300).

### 1.4 Áudio por aplicativo 🟢

- Usa `AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK` com `PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE`. O PID vem da janela.
- Só é ativado no **build 19041+**. O comentário no código diz: *"MS says 20348, but process filtering seems to work earlier"*.
- Os timestamps são a posição QPC do WASAPI × 100 (em ns).
- Fontes: [`win-wasapi.cpp`](https://github.com/obsproject/obs-studio/blob/c5bcbca63fc32f8341c08e1f921b2b81ec46be00/plugins/win-wasapi/win-wasapi.cpp#L636-L750), [`plugin-main.cpp`](https://github.com/obsproject/obs-studio/blob/c5bcbca63fc32f8341c08e1f921b2b81ec46be00/plugins/win-wasapi/plugin-main.cpp#L41-L63).

### 1.5 Relógio e replay buffer 🟢

- **Todo o tempo do OBS é QPC.** `os_gettime_ns()` usa `QueryPerformanceCounter`, e os pacotes do encoder recebem tempo de sistema, com o comentário *"we use system time here to ensure sync with other encoders"*. O OBS não sincroniza relógios entre máquinas. Fontes: [`platform-windows.c` L391–396](https://github.com/obsproject/obs-studio/blob/c5bcbca63fc32f8341c08e1f921b2b81ec46be00/libobs/util/platform-windows.c#L391-L396), [`obs-encoder.c` L1400–1416](https://github.com/obsproject/obs-studio/blob/c5bcbca63fc32f8341c08e1f921b2b81ec46be00/libobs/obs-encoder.c#L1400-L1416).
- **Replay buffer:**
  - é uma fila (*deque*) **na RAM** de pacotes codificados com contagem de referência. O padrão do plugin é 15 s / 500 MB; o da interface, 20 s / 512 MB;
  - descarta GOPs inteiros pelo começo;
  - "Salvar" só **anota `save_ts = agora (QPC)`**. O arquivo é gerado quando chega o primeiro pacote com `sys_dts_usec ≥ save_ts`. Isso **espera a latência do encoder**, mas **não grava nada depois do aperto**;
  - **não existe pós-roll nativo.** As únicas funções são `save()` e `get_last_replay`;
  - fontes: [`obs-ffmpeg-mux.c` L917–952](https://github.com/obsproject/obs-studio/blob/c5bcbca63fc32f8341c08e1f921b2b81ec46be00/plugins/obs-ffmpeg/obs-ffmpeg-mux.c#L917-L952) e [L1220–1250](https://github.com/obsproject/obs-studio/blob/c5bcbca63fc32f8341c08e1f921b2b81ec46be00/plugins/obs-ffmpeg/obs-ffmpeg-mux.c#L1220-L1250).
- Pós-roll no OBS só existe por gambiarra: scripts com timer, Advanced Scene Switcher ou AutoHotkey ([fórum](https://obsproject.com/forum/threads/replay-buffer-record-after-hotkey.152956/)).

**Resposta para o OBS:** o método mais eficiente do OBS **não é gravação de tela**, é **injeção de DLL** (Game Capture). A captura de janela por WGC existe no OBS, mas o modo automático prefere BitBlt para a maioria das janelas. O WGC é o caminho recomendado pelo próprio OBS **quando o hook é bloqueado**.

---

## 2. Medal: pesquisa a fundo

A documentação pública do Medal **não nomeia as APIs**. Por isso a pesquisa juntou o que o próprio Medal escreve no suporte, logs reais do Medal publicados por terceiros e análise estática do instalador de 2026 feita por terceiros. `medal.tv` e `support.medal.tv` estavam bloqueados no ambiente da pesquisa, então os textos de suporte vieram de trechos de busca.

### 2.1 O que o próprio Medal diz 🟢

- O artigo de suporte sobre antivírus diz que **"o Medal injeta nos seus jogos para dar o overlay e capturar a sua gameplay"**, e liga os falsos positivos de antivírus a essa técnica ([How to whitelist Medal in your anti-virus](https://support.medal.tv/support/solutions/articles/48001166446)).
- O artigo de clipes pretos fala em overlays e gravadores que "interferem no hook do Medal". A lista de programas para fechar inclui **OBS, FACEIT Anti-Cheat e Discord** ([Black clips / stuck in 1 frame](https://support.medal.tv/support/solutions/articles/48000922110-black-clips-stuck-in-1-frame)).
- **"Advanced Window Capture"** grava mesmo com o jogo minimizado e evita o erro "Game out of Focus". Ele mostra a **borda amarela** do Windows e **não funciona em fullscreen exclusivo** ([What is Advanced Window Capture?](https://support.medal.tv/support/solutions/articles/48001171330-what-is-advanced-window-capture-)).
- **"Force Window Capture"** só funciona em janela ou sem bordas.

### 2.2 O que os logs e binários mostram 🟡

- **Logs do Medal 3.696 (ago/2023), publicados por terceiros** ([MedalLog20230803](https://github.com/lokritshok/FinalAssignmentVisualStudio/blob/56a6de5bb9724af85d50ffe30434706cbad5a4f5/Medal/MedalLog20230803.txt), [20230804](https://github.com/lokritshok/FinalAssignmentVisualStudio/blob/56a6de5bb9724af85d50ffe30434706cbad5a4f5/Medal/MedalLog20230804.txt), [20230809](https://github.com/lokritshok/FinalAssignmentVisualStudio/blob/56a6de5bb9724af85d50ffe30434706cbad5a4f5/Medal/MedalLog20230809.txt)):
  - **Hook derivado do OBS:** aparecem `[ESM State]: OBSInjectionState`, `Injecting into process`, `[OBSCaptureState] Shared texture success` e `GameRecordingSuccessfullyStarted InjectionCapture`. O bloco de *offsets* `[d3d8]/[d3d9]/[dxgi]` é **idêntico, linha por linha,** ao formato que o [`get-graphics-offsets.c` do OBS](https://github.com/obsproject/obs-studio/blob/c5bcbca63fc32f8341c08e1f921b2b81ec46be00/plugins/win-capture/get-graphics-offsets/get-graphics-offsets.c#L33-L45) imprime. As DLLs se chamam `medal-hook32.dll` / `medal-hook64.dll`.
  - **Advanced Window Capture = WGC:** a requisição interna se chama literalmente `set.windowsGraphicsCapture`, seguida de "Saving Advanced Window Capture".
  - **Captura de janela "padrão" = DXGI:** aparecem `Capture mode: DXGI`, `WindowCaptureStandard`, um retângulo "Capture area" e a imagem `inactiveGame.png` quando o jogo perde o foco. ⚪ Isso é muito provavelmente Desktop Duplication recortada na janela, o que explicaria o erro "Game out of Focus".
  - **Áudio:** WASAPI por processo ("GAO"), com falhas **intermitentes** (`0x8000FFFF`, 4 ocorrências entre muitos sucessos) no Windows 10 19045.
- **Análise estática do recorder v2638.2751.1 (set/2026), feita por terceiros** ([medal-cross-platform](https://github.com/RyanTheTechMan/medal-cross-platform/blob/6b660dacc116fe09774590df828d40482202de6f/research/evidence/recorder_metadata.json)):
  - ainda existem as classes `MedalEncoder.OBS.HookInterface`, `BaseHookInterface` e `GraphicsOffsets`;
  - existe uma pasta *Host* com os injetores e o arquivo `Licenses/obs_source_offer.txt` (oferta de código-fonte GPL das partes vindas do OBS);
  - nas configurações padrão, `PreferGameCapture=true`, `ForceWindowCapture=false` e `AdvancedWindowCapture=false`;
  - o motor nativo `scope.dll` (D3D11/DXGI + FFmpeg) tem buffers de replay em **memória, disco ou mmap**, áudio separado por processo (jogo, Discord.exe) e atalhos via `SetWindowsHookEx`.
- **História do motor:** começou com FFmpeg CLI, depois usou o SDK Medialooks MFormats ([estudo de caso](https://blog.medialooks.com/medal-captures-game-moments-with-mformats/)) e hoje usa um motor próprio (`scope.dll`).
- **Pós-roll:** o atalho manual só salva o passado. A *Game API* do Medal, porém, tem `captureDelayMs`: *"milliseconds Medal waits after the request before snapshotting the replay buffer"* (transcrição da especificação OpenAPI, [GMEXT-Medal](https://github.com/YoYoGames/GMEXT-Medal/blob/2284effa4b093406e6fb5dd40d934862389830db/spec/medal-openapi.yaml#L100-L108)). É exatamente a técnica de "esperar e depois cortar" que vamos usar.
- **Armazenamento na nuvem:**
  - os clipes enviados saem de `cdn.medal.tv` com URL assinada e expirável (`exp=…~hmac=…`);
  - os links privados expiram em 14 dias e renovam ao copiar;
  - o upload automático vem por padrão **depois que o jogo fecha**, para não pesar na banda durante a partida ([expiring links](https://support.medal.tv/support/solutions/articles/48001259493-how-to-upload-clips-expiring-links), [auto-upload FAQ](https://support.medal.tv/support/solutions/articles/48001258674-auto-upload-faq)).

**Resposta para o Medal (confiança: alta para o padrão de 2023 e média para o comportamento padrão em 2026):**
- O **método padrão** do Medal é um **hook injetado, derivado do OBS** (textura compartilhada), e o próprio suporte do Medal admite que injeta.
- O **"Advanced Window Capture" é exatamente o WGC** que propomos. É opcional e desligado por padrão, e o Medal o recomenda para jogos como CS2.
- Também há uma captura de janela "padrão" baseada em DXGI e uma captura de tela inteira (Desktop Capture).

### 2.3 Como fechar a questão do Medal em 2026 (opcional)

Faça um teste "caixa-preta" num PC de vocês com Windows 11 e o Medal atual nas configurações padrão:

1. Abra um jogo comum DX11/DX12, depois CS2, Valorant e uma partida da Gamers Club/FACEIT.
2. Em cada um, veja se a DLL foi carregada no jogo: `tasklist /m medal-hook64.dll`, ou Process Explorer → Find → "medal-hook".
3. Leia o log do recorder em `%AppData%\Medal` e procure `OBSInjectionState`, `InjectionCapture`, `WindowCaptureStandard`, `Capture mode: DXGI` e `set.windowsGraphicsCapture`.
4. Ligue e desligue o Advanced Window Capture e o Experimental Capture, e compare os logs.

---

## 3. Outras ferramentas

| Ferramenta | Método | Confiança | Observação |
|---|---|---|---|
| **NVIDIA ShadowPlay / NVIDIA App** | **NvFBC** (captura no driver) → NVENC | 🟡 (o log do ShadowPlay mostra `NvFBCH264GrabFrame`) | A NVIDIA **descontinuou o NvFBC para terceiros no Windows 10+** e recomenda DDA/WGC. Na GeForce, ele só funciona com a chave privada da NVIDIA e uma ativação como admin, e apenas um cliente por vez ([rbuf](https://github.com/r3clusionn/rbuf), [AlwaysShadow #20](https://github.com/Verpous/AlwaysShadow/issues/20)). **Não serve para nós.** |
| **AMD Adrenalin (ReLive)** | Captura no driver | 🟢 para a API pública | O AMF SDK expõe uma **API de captura de tela** (*Display Capture*) que a AMD diz ser melhor que DDA, mas só do **monitor inteiro** ([AMF Display Capture](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/blob/8c648005e07d4309033282bfd9947df2c7e76104/amf/doc/AMF_Display_Capture_API.md)). O que o ReLive usa internamente não é documentado. |
| **Xbox Game Bar** | Interno do Windows | 🟢 | Não há API para um app terceiro gravar outros jogos (`Windows.Media.AppRecording` só grava o próprio app). |
| **Steam Game Recording** | ⚪ Provavelmente o hook do overlay da Steam (só funciona onde o overlay funciona) | 🟡 | Grava **continuamente em disco** (segmentos) e o usuário escolhe início e fim **depois**, na linha do tempo ([FAQ](https://help.steampowered.com/faqs/view/23B7-49AD-4A28-9590), [Timeline API](https://partner.steamgames.com/doc/features/timeline)). |
| **Discord (Go Live / Clips)** | Por padrão, DLL assinada injetada (tecnologia do overlay). **WGC** é opção e deve virar padrão no Windows 11. | 🟡 (artigo de suporte não reconferido) | [Artigo de suporte](https://support.discord.com/hc/en-us/articles/9410427556375--Windows-Capturing-Application-Window-for-Screen-Share-and-Go-Live) |
| **Overwolf / Outplayed / Insights Capture** | Motor baseado no OBS, com **game capture** (textura compartilhada) por padrão | 🟢 (documentação Overwolf) | A API tem `capture(pastDuration, futureDuration)`, ou seja, **pós-roll nativo**, com buffer na memória ([overwolf.d.ts](https://github.com/overwolf/types/blob/master/overwolf.d.ts#L873-L1010)). |
| **SteelSeries Moments** | O "Capture Mode" do Windows 11 rotula a captura de jogo como **"Game Capture (WGC)"** | 🟡 | [Suporte SteelSeries](https://support.steelseries.com/hc/en-us/articles/34379253751309-What-is-Moments-Capture-Mode) |
| **Allstar** | **Não grava a tela:** renderiza o clipe no servidor a partir do **demo da partida** (CS2, Dota 2, LoL, Fortnite) | 🟡 | Dá sincronia perfeita entre POVs (é o mesmo demo), mas **sem voz do Discord** e não é a tela real do jogador ([como funciona](https://allstar.gg/howitworks), [Gamers Club](https://help.allstar.gg/hc/en-us/articles/15136805222423-Claim-Clips-Gamers-Club)). |
| **Eklipse** | Não grava localmente; corta VODs da Twitch na nuvem | 🟡 | — |

---

## 4. Eficiência: o que se sabe de verdade

- **Não existe benchmark independente e controlado** comparando hook × WGC × DDA em FPS de jogo. Os números exatos que circulam em blogs não são confiáveis.
- **Dados concretos encontrados:**
  - **OBS PR #2208 (2020):** o WGC custa ~200–800 µs de CPU por frame, contra ~300–500 µs do BitBlt, e um pouco menos de GPU.
  - **rbuf (um único PC, um desenvolvedor):**
    - jogo a 1.218 fps sem gravar → 1.164 com NvFBC, 1.158 com ShadowPlay e 1.071 com o "display capture" do OBS (o OBS também gastou 20,4% de CPU no `dwm.exe`);
    - **no próprio processo do gravador**, o WGC gastou ~1,93% de CPU contra 1,87% do NvFBC, num teste separado. Isso **não** é uma medição de FPS de jogo.
- **Ranking aproximado, do mais leve ao mais pesado para o jogo** (⚪ inferência a partir das evidências):
  1. captura no driver (NvFBC / AMD);
  2. hook + textura compartilhada;
  3. **WGC de janela**;
  4. WGC de monitor / DDA;
  5. BitBlt.
- **Efeitos colaterais do WGC a medir** 🟡:
  - pode tirar o jogo do modo *independent flip* em PCs sem MPO, com VRR ligado e cursor de hardware visível;
  - entrega frames no ritmo do DWM;
  - pode limitar a ~50–60 fps se `MinUpdateInterval` não for configurado ([Win32CaptureSample #82](https://github.com/robmikh/Win32CaptureSample/issues/82), [Sunshine](https://github.com/LizardByte/Sunshine/blob/0594f62d4cc6179aa055f0363043adbc8849b62b/src/platform/windows/display_wgc.cpp#L140-L168)).

**Conclusão:**
- Os dois métodos mais eficientes que existem (driver e hook) **não estão disponíveis** de forma legítima e segura para um app novo:
  - o NvFBC é restrito e descontinuado;
  - o hook depende de uma exceção nos anti-cheats que só OBS e similares têm, e o CS2 em modo confiável não aceita injeção de terceiros.
- **Entre os métodos que podemos usar, o WGC de janela é o mais eficaz.** Discord e SteelSeries já o oferecem. No Medal ele é só um modo opcional; o padrão do Medal é o hook injetado, sem borda amarela.
- A comprovação final vem do **nosso benchmark com PresentMon** na Fase 0.
