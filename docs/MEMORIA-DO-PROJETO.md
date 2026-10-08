# Memória do projeto DuoClip

> Arquivo de memória para migrar o projeto para outro ambiente sem perder contexto.
> Resume **tudo o que foi pedido, pesquisado, decidido e implementado** na conversa de 07–08/10/2026
> (sessão Claude Code na nuvem). Mantenha este arquivo atualizado a cada decisão importante.
>
> - Repositório: `nicolaspercio1/duoclip` (antes `culturagrowth/code`, apagado em 08/10/2026) · branch de trabalho: `claude/sync-gameplay-clip-app-xpgfwx`
> - Idioma de trabalho: **português do Brasil**. O código e os comentários ficam em inglês.
> - **Regras para qualquer IA:** [`AGENTS.md`](../AGENTS.md) · **quem faz o quê:** [`docs/TAREFAS.md`](TAREFAS.md)
> - Documentos principais:
>   - [`docs/pesquisa-app-clipes-sincronizados.md`](pesquisa-app-clipes-sincronizados.md): pesquisa e arquitetura completas;
>   - [`docs/anexo-metodos-de-captura-obs-medal.md`](anexo-metodos-de-captura-obs-medal.md): evidências de como OBS e Medal capturam;
>   - `crates/*/SPEC.md` e `worker/SPEC.md`: contratos de implementação de cada módulo.

---

## 1. O que o usuário quer (pedido original)

Um app **para Windows** (10 e 11, "universal": qualquer jogo, qualquer GPU) em que um grupo de amigos joga o mesmo jogo
e o app:

1. grava a tela do jogo continuamente, **da forma mais eficiente possível**, sem afetar a qualidade nem o FPS (referências: Medal, OBS);
2. quando acontece algo engraçado, alguém aperta **um botão de clipe**, e o app clipa **a tela de todos os amigos**,
   **sincronizadas exatamente no tempo**;
3. inclui **alguns segundos antes e alguns segundos depois** do aperto;
4. abre uma **pré-visualização** para escolher onde o clipe começa e termina;
5. é **seguro**, captura **só o jogo com o som do jogo** e **o som das conversas do Discord**.

Uso **privado entre amigos**, sem plano de lançar ao público por enquanto.

## 2. Linha do tempo das decisões do usuário

| # | O usuário disse | O que ficou decidido |
|---|---|---|
| 1 | Pesquisa inicial na internet para criar o app | Documento v1 de pesquisa e arquitetura |
| 2 | "Usar um **relógio global** sincronizado dentro do app em vez do relógio do computador" | Relógio Global DuoClip (seção 4.3 abaixo) |
| 3 | "Verificar a gravação um pouco **para frente** do tempo do aperto" | Pós-roll "fixar e coletar" (4.2) |
| 4 | "Usar um **sistema de bucket** para os clipes temporários" | Bucket na nuvem (Cloudflare R2) |
| 5 | "Verificar se a gravação de tela é a mais eficaz e o que **Medal/OBS** usam; se não achar, pesquisar mais a fundo" | Pesquisa a fundo, com código do OBS e logs do Medal (seção 5) |
| 6 | "**Bucket é na nuvem mesmo**" | Confirmado: R2 |
| 7 | "**Jurídico e consentimento não têm problema**, é só entre amigos" | Sem tela de veto. O pareamento entre amigos vale como autorização. LGPD/ECA Digital ficam só como nota para o caso de o app um dia ser público. |
| 8 | "**Windows 10** como falei; o Medal **não tem borda amarela** e é super eficiente, não fica lagado" | Suporte completo ao Windows 10, **sem borda amarela**, com eficiência no nível do Medal |
| 9 | "Por que você falou que era o modo do Medal sendo que não é?" | **Correção:** eu tinha dito (v1) que o Medal usava WGC. **Errado.** O padrão do Medal é um **hook injetado** (derivado do OBS). O WGC no Medal é só o modo opcional "Advanced Window Capture". O documento foi corrigido. |
| 10 | Pediu a diferença entre captura sem injeção e hook injetado | Explicado (seção 4.1) |
| 11 | "Pode ser o plano: **sem injeção por padrão** e o **hook como modo opcional** — deixa programado mas implementa mais pra frente" | **Decisão final de captura** (4.1) |
| 12 | "Pode seguir; use **agentes mais eficientes para código fácil** e **agentes mais avançados para partes difíceis**" | Implementação começou: sonnet para as partes fáceis, opus para as difíceis, cada parte com revisão adversarial |
| 13 | "Armazene em um arquivo de memória tudo que for importante" | Este arquivo + `CLAUDE.md` na raiz |
| 14 | "Pode atualizar tudo da memória de novo, agora que acabou de aplicar" | Memória, documentos e anexo atualizados com a Fase B1 parcial e a pesquisa da rodada 3. Pesquisas brutas e scripts dos agentes guardados no repositório. |
| 15 | "Crie também um prompt para eu mandar para o meu Claude Code no CLI para puxar tudo e realizar o teste do app" | [`docs/PROMPT-CLAUDE-CODE-LOCAL.md`](PROMPT-CLAUDE-CODE-LOCAL.md) |
| 16 | "Quero que funcione com quantos amigos eu quiser jogando junto; tenho 2 grupos diferentes, jogo com um num dia e com o outro no outro; às vezes 6, às vezes 3; sem um interferir no outro" | **Grupos e sessões** (4.7): vários grupos isolados, **sessão automática** e sessões de **até 8 pessoas** |
| 17 | "Tenho outra IA (GPT 6.1 SOL) e quero delegar tarefas para ela também; deixe tudo adaptável para usar as duas" | Regras neutras em [`AGENTS.md`](../AGENTS.md) (o `CLAUDE.md` importa); quadro [`docs/TAREFAS.md`](TAREFAS.md) com dono e branch por tarefa (`claude/…`, `gpt/…`); modelo [`docs/PROMPT-DELEGAR-TAREFA.md`](PROMPT-DELEGAR-TAREFA.md); revisão cruzada antes do merge |
| 18 | "Já que vamos usar 2 IAs, melhor uma revisar o que a outra fez: você revisa o GPT e o GPT revisa você, em vez de você fazer, revisar e depois mandar pra ele" | **Revisão cruzada obrigatória**: quem implementa não revisa (nem com subagentes da mesma IA). Revisões em [`docs/revisoes/`](revisoes/), branch por tarefa até o veredito "aprovado" |

## 3. Preferências do usuário

- Respostas em **português**, diretas, sem enrolação. Quando eu erro, ele quer que eu admita e corrija.
- Prioridades: **eficiência como o Medal**, **sem borda amarela**, **Windows 10 e 11**, **seguro** (sem risco de ban).
- Uso privado entre amigos: nada de burocracia jurídica nem telas de consentimento.
- Divisão de trabalho com agentes: os **mais rápidos e baratos (sonnet)** ficam com o código simples; os **mais avançados (opus)**
  ficam com as partes difíceis ou sofisticadas. Cada parte passa por revisão adversarial com correção.

## 4. Arquitetura decidida (resumo; detalhes no documento de pesquisa)

### 4.1 Captura de vídeo — DECISÃO FINAL

- **Padrão: captura sem injeção** (o app não encosta no processo do jogo, então não há risco com anti-cheat).
  - **Windows 10:** **DXGI Desktop Duplication recortado na janela do jogo**. Não tem borda amarela e é o mesmo método sem injeção que o
    Medal usa na captura de janela "padrão". Cuidados:
    - grava o que aparecer por cima do jogo. Quando o jogo perde o foco, o app grava uma tela "jogo fora de foco" (`SetWinEventHook(EVENT_SYSTEM_FOREGROUND)`);
    - recria a duplicação em `DXGI_ERROR_ACCESS_LOST`;
    - roda na GPU dona do monitor.
  - **Windows 11:** WGC sem borda (`RequestAccessAsync(Borderless)` + `IsBorderRequired(false)`), se o teste de empacotamento
    confirmar que funciona sem MSIX; senão, o mesmo Desktop Duplication recortado.
  - **Confirmado pela rodada 3:** no Windows 10 **não há como tirar a borda do WGC**. No 19045, `IsBorderRequired` dá `E_NOINTERFACE`, então sempre é preciso detectar a API antes.
    O Medal não tem borda porque usa hook ou Desktop Duplication recortado. *DWM shared surface* (`DwmGetDxSharedSurface`) fica só como backend experimental.
  - **Práticas de desempenho copiadas do Medal:**
    - imagem só na GPU;
    - encoder de hardware;
    - buffer na RAM;
    - processo em prioridade alta, `SetMaximumFrameLatency` e MMCSS;
    - *dirty rects*;
    - `WDA_EXCLUDEFROMCAPTURE` nas nossas janelas.

    **Não copiar:** a prioridade de GPU *realtime* (exige admin) nem o desligamento do Modo de Jogo do Windows.
- **Opcional e futuro (Fase 6), FORA DO MVP pela pesquisa da rodada 3: hook injetado no estilo Medal.** Dos jogos populares no Brasil, só o **Minecraft Java** não tem anti-cheat no cliente.
  Regras extras:
  - lista de bloqueio fixa (CS2, Valorant, LoL, Fortnite, Roblox etc.);
  - o hook se desliga sozinho se um anti-cheat de kernel estiver rodando;
  - nunca hooks globais nem *layer* Vulkan implícita;
  - *kill switch* remoto.

  Detalhes: Desligado por padrão e ligado jogo a jogo, **só em jogos sem anti-cheat**
  (Minecraft, single-player...). Bloqueado em Vanguard, EAC, BattlEye, VAC/CS2, FACEIT, Gamers Club, Ricochet e Hyperion. Componente
  separado e assinado (`duoclip-hook`). Volta sozinho para a captura sem injeção se falhar. Se for derivado do OBS (GPL-2), vai como
  componente separado com o código-fonte disponível, como o Medal faz.
- **Arquitetura plugável:** `trait CaptureBackend` (dda_crop, wgc e, no futuro, hook). Todo método entrega uma textura D3D11 com timestamp QPC.
  Um **banco de jogos** guarda o anti-cheat de cada jogo, os métodos permitidos e o padrão.
- **Diferença explicada ao usuário:**
  - o hook pega o frame dentro do jogo: é o mais leve, sem borda e funciona em tela cheia exclusiva, mas traz risco com anti-cheat e de crash;
  - a captura sem injeção pega a imagem já montada pelo Windows: é segura, com custo um pouco maior, provavelmente pequeno.
    A compressão é feita pelo chip de vídeo nos dois casos.
- Outros detalhes:
  - resolução de saída fixa (sem reiniciar o encoder);
  - HDR via `IDXGIOutput6::GetDesc1` com tone-map;
  - cursor fora da imagem;
  - respeitar `WDA_EXCLUDEFROMCAPTURE`;
  - jogos rodando como admin exigem o app como admin.

### 4.2 Codificação, buffer e pós-roll

- Encoder de hardware: NVENC, AMF ou QSV (FFmpeg libavcodec em build LGPL), com a imagem sempre na GPU.
  - **H.264** (HEVC e AV1 opcionais), saída fixa a 60 fps, VBR/CQP com teto de ~30–50 Mbps em 1080p60;
  - **GOP fechado de 1 s, sem B-frames**;
  - a prévia de 720p é **transcodificada sob demanda**, sem um segundo encode contínuo;
  - GPU sem encoder (RX 6500 XT/6400, GT 1030): usar a GPU integrada ou x264 720p30 com aviso.
- **Ring buffer na RAM** de pacotes codificados (modelo do OBS): 60 s, teto de ~600 MB, descarte de GOPs inteiros.
- **Pós-roll "fixar e coletar":** no pedido, o app fixa (com `Arc`) os pacotes desde o keyframe ≤ início, continua coletando até todas as
  faixas passarem do fim, e finaliza (timeout = fim + 3 s, ou o jogo fechou). Esperar e salvar depois não funciona, porque o começo seria descartado.
  - padrões: **30 s antes, 10 s depois, margens ocultas de +2 s e +2 s** (alargadas para máx(2 s, 3ε) com sincronia ruim);
  - corte inicial no editor: T−10 s → T+5 s;
  - máximo de 180 s por clipe (com extensões) e 4 clipes ativos ao mesmo tempo;
  - extensão se a mesma pessoa apertar de novo; dois apertos de pessoas diferentes compartilham os pacotes;
  - clipe parcial com metadados de cobertura;
  - **buckets locais**: MP4 fragmentado por GOP em `%LOCALAPPDATA%\DuoClip\buckets\<clip_id>\` (sobrevive a crash);
  - a regra de fragmento pronto é a do OBS mp4-mux (próximo keyframe visto e todo áudio passou dele).
- Precedentes pesquisados:
  - OBS: só salva o passado (`save_ts` em QPC);
  - Medal Game API: `captureDelayMs`;
  - Overwolf: `capture(past, future)`;
  - Steam: grava em disco e você escolhe o trecho depois.

### 4.3 Relógio Global DuoClip

- **Nunca** usa nem altera o relógio do Windows. Num PC doméstico, o Windows sincroniza a cada ~9,1 h (`MaxPollInterval = 2^15`) e dá saltos acima de 1 s.
- **Base local:** QPC, que no Rust é o `Instant`. **Escala global:** UTC em ns. `AppClock(q) = ref_utc + (q − ref_qpc)·(1 + rate)`. O offset e a frequência
  são estimados continuamente (filtro de menor RTT, regressão ponderada, Marzullo entre fontes).
- **Fontes:** NTP.br `a`–`e.st1.ntp.br` (césio do Observatório Nacional; o NIC.br diz que todos têm NTS) e `time.cloudflare.com` (NTS, sem smear).
  **Nunca** Google ou AWS (fazem leap smear), nem time.windows.com ou pool.ntp.org como padrão.
  - O MVP usa **SNTP** (marcado como "não autenticado"); NTS vem depois (crates `ntp-proto`, ou um fork do `rkik-nts` usando QPC).
  - Rajada de 6 consultas no início, depois 64 s, mínimo 15 s, respeitando KoD.
- **Refino P2P:** pings estilo NTP com 4 timestamps em QPC, num canal WebRTC não confiável e não ordenado, mais checagem cruzada global × P2P.
- **Janela de captura:** usa o mapeamento **congelado no pedido**. **Alinhamento no editor:** usa a regressão de dois lados (amostras antes e depois do clipe).
- O aperto é enviado como **timestamp global** (`hotkey_utc_ns`), então o atraso da mensagem não importa.
- **Metas:** 🟢 ≤ 8 ms, 🟡 ≤ 33 ms, 🔴 acima disso; P95 ≤ 16,7 ms (1 frame a 60 fps) em fibra.
  Estados: SINCRONIZADO, DEGRADADO, HOLDOVER, SEM SINCRONIA. Só faz slew (nunca volta para trás); saltos só criam um novo `epoch`.
- **Limite físico:** o netcode faz cada jogador ver o mesmo evento em momentos diferentes (~100 ms no Source). Por isso o editor tem ajuste fino (±1 frame).
- Calibrar o atraso áudio/vídeo por fonte (o `SystemRelativeTime` é o horário de composição do DWM; foram medidos de 18 a 44 ms).

### 4.4 Áudio

- WASAPI *process loopback* (`AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK` + `INCLUDE_TARGET_PROCESS_TREE`), igual a OBS e Medal.
  - **Oficialmente build 20348+**, mas o OBS usa desde o 19041. Falhas intermitentes no Windows 10 19045 → tentar de novo e, em último caso, usar o loopback do dispositivo.
- Faixas: **jogo** (PID da janela), **Discord** (processo raiz `Discord.exe`/PTB/Canary, com a árvore de processos), **microfone** (opcional, desligado).
- O Discord não toca a sua própria voz para você. Regras do editor: preferir os mics sincronizados, nunca tocar a mesma voz duas vezes,
  e compensar o atraso do Discord.

### 4.5 Bucket na nuvem e rede

- **Cloudflare R2** (sem custo de download). **Para o grupo de amigos fica no plano gratuito (US$ 0).** Para comparação, com 1.000 usuários seria ≈ US$ 2–4/mês
  no R2, contra US$ 140–220 em S3, GCS ou Supabase com servidor em São Paulo.
- **Bucket primeiro, P2P como acelerador** (o bucket funciona com o amigo offline ou atrás de CGNAT, sem precisar de TURN para os arquivos).
- **Criptografia ponta a ponta:** chave aleatória por clipe; AES-256-GCM por bloco, com AAD = clip, pov, qualidade, índice e é_último;
  manifesto cifrado; a chave só vai para os amigos pareados. A Cloudflare só vê bytes cifrados.
- **Unidade de transferência:** fragmentos fMP4 por GOP, agrupados em **objetos de ~4–8 MiB**:
  - `clips/{crew}/{clip}/{pov}/{proxy|full}/{index:06}.bin`;
  - mais o `manifest.bin`.
- Prévia de 720p primeiro (~16,5 MB). Depois, só o trecho escolhido em qualidade total (~60 MB). Upload limitado para não aumentar o ping.
- **Worker** (Cloudflare):
  - autenticação de dispositivos Ed25519 (requisições assinadas, proteção contra replay);
  - crews (grupos de amigos) com convites;
  - URLs pré-assinadas PUT/GET de 15 min;
  - cotas;
  - exclusão ao exportar, varredura cron de hora em hora e regra de ciclo de vida de 3 dias como reserva.
- **WebRTC DataChannel** com os canais `controle`, `relógio` e `dados`; identidades Ed25519; o pareamento é por código de convite.

### 4.6 Editor, stack e requisitos

- **Editor:**
  - POVs alinhados pelo relógio global;
  - marcador do aperto, faixa de incerteza e lacunas;
  - layouts lado a lado, 9:16, PiP, cortes alternados e arquivos separados;
  - mixer e ajuste fino;
  - exportação na GPU.
- **Stack:**
  - Rust no núcleo (`windows-rs`, FFmpeg LGPL);
  - **Tauri 2** (WebView2) na interface, com WebCodecs no editor;
  - `webrtc-rs` ou `libdatachannel` para o P2P;
  - **Cloudflare Worker + R2 + D1** no backend.
- **Atalho:** `RegisterHotKey`, sem hook global de teclado.
- **Requisitos:**
  - Windows 10 22H2 (~25–27% do Brasil; atualizações ESU para consumidores **até 12/10/2027**) e Windows 11;
  - GPU com encoder H.264;
  - ~0,6 GB de RAM livre;
  - upload de 5 Mbps ou mais.
- **Distribuição entre amigos:** instalador próprio basta. A Microsoft Store é opcional (cadastro grátis desde 09/2025).

### 4.7 Grupos e sessões (decisão de 08/10/2026)

- **Não é um app de dupla.** Cada pessoa pode estar em **vários grupos** ("crews"; o Worker já tem `crew_members` muitos-para-muitos).
  Os grupos são **isolados**: o clipe pertence a um grupo (`crew_id`), as chaves no R2 começam com `clips/{crew}/` e só membros recebem as URLs.
- **Sessão de jogo automática** (ainda não implementada, entra na Fase C):
  - a sessão junta sozinha os membros do **mesmo grupo** que estão com o app aberto **e no mesmo jogo** (pelo banco de jogos);
  - o clipe vai **só para quem está na sessão**: se 3 dos 6 estão jogando, só esses 3 participam; quem não está não recebe nada;
  - se a pessoa está em 2 grupos com membros online nos dois, o app **pergunta** qual usar e lembra a escolha;
  - um amigo que entra no meio da sessão passa a participar dos próximos clipes (não dos que já estavam coletando).
- **Tamanho:** sessões de **até 8 pessoas** são suportadas e testadas. Os grupos de uso real têm de 3 a 6.
  - Rede: P2P entre todos (até 28 conexões) só para controle e relógio. Os arquivos vão pelo bucket, e cada pessoa sobe o próprio POV uma vez.
    Download por clipe com 6 pessoas ≈ 5 × 16,5 MB (prévias 720p).
  - Relógio: o refino P2P usa todos os pares disponíveis da sessão.
  - Editor: layouts para N POVs (grade, foco + miniaturas, cortes alternados) e a regra de áudio de 3+ pessoas (uma faixa do Discord como "mestre").

## 5. Principais fatos da pesquisa (para não refazer)

- **OBS** (código conferido, commit `c5bcbca`):
  - "Game Capture" = injeção de DLL (`graphics-hook64.dll` via `SetWindowsHookEx` ou `CreateRemoteThread`, hook em Present, textura compartilhada).
    Os anti-cheats toleram por causa do **certificado** do OBS;
  - "Window Capture" automático prefere **BitBlt**; WGC ("Modern") só para algumas classes de janela;
  - "Display Capture" = DXGI Desktop Duplication; WGC em notebook híbrido;
  - áudio por aplicativo = process loopback (≥ 19041);
  - todo o tempo é QPC;
  - replay buffer = fila na RAM, **sem pós-roll**.
- **Medal:**
  - o suporte do próprio Medal diz que ele **"injeta nos seus jogos"**;
  - logs de 2023 (de terceiros) mostram `OBSInjectionState` e offsets no formato do OBS (hook **derivado do OBS**);
  - "**Advanced Window Capture" = WGC** (`set.windowsGraphicsCapture`), opcional, com borda amarela;
  - a captura de janela "padrão" é **DXGI** (`Capture mode: DXGI`), sem borda;
  - recorder de 2026 (análise de terceiros): `PreferGameCapture=true`, classes `MedalEncoder.OBS.*`, motor `scope.dll` com FFmpeg, áudio separado por processo;
  - pós-roll só na Game API (`captureDelayMs`).
- **Outros:**
  - NVIDIA ShadowPlay: NvFBC (descontinuado e restrito para terceiros);
  - AMD: captura no driver; a API pública do AMF captura só o monitor;
  - Steam: provavelmente o hook do overlay; grava em disco e o trecho é escolhido depois;
  - Discord: DLL injetada por padrão, WGC como opção;
  - Overwolf/Outplayed: motor do OBS;
  - SteelSeries: "Game Capture (WGC)";
  - Allstar: renderiza a partir do demo da partida (possível modo "sincronia perfeita" para o CS2 no futuro).
- **Eficiência:** não existe benchmark independente. O OBS mediu o WGC em ~200–800 µs de CPU por frame. Vamos medir com **PresentMon**,
  incluindo **comparar com o Medal ligado no mesmo jogo**.
- **Borda do WGC:** só sai no build **20348+** (Windows 11). A Microsoft exige a capability `graphicsCaptureWithoutBorder` no manifesto de pacote.
  Não está documentado se funciona num app sem pacote (protótipo de 1 dia: sem pacote / configuração do Win11 / MSIX).
- **Preços conferidos (out/2026):**
  - R2: US$ 0,015/GB-mês, download grátis, plano grátis de 10 GB, 1 milhão de operações A e 10 milhões de B;
  - S3 sa-east-1: US$ 0,0405/GB e US$ 0,15/GB de saída;
  - GCS São Paulo: US$ 0,035 e US$ 0,12, com soft delete de 7 dias ligado por padrão;
  - Supabase Pro: US$ 25, com 250 GB de saída e US$ 0,09/GB depois; sem expiração de objetos.
- **Rodada 3 (08/10/2026), captura sem borda e hook:**
  - Borda do WGC no Win10: impossível de forma documentada (contrato v12, build 20348+). No 19045 dá `E_NOINTERFACE`, e a configuração do Windows para isso só existe no Windows 11.
  - Medal sem borda no Win10: hook (OBS) ou "WindowCaptureStandard" = Desktop Duplication recortado com `inactiveGame.png`.
    Padrões de 2026: `PreferGameCapture=true`, 720p60, 15 Mbps, VFR, buffer na RAM, Modo de Jogo desligado.
  - **Anti-cheat por jogo:** tabela no anexo, seção 5. Só o Minecraft não tem. VAC já baniu por hook inofensivo (AMD Anti-Lag+, 2023).
    O Rocket League tem EAC desde 28/04/2026. **A FACEIT encerra o suporte ao Windows 10 em 14/10/2026.**
  - O Discord refez o overlay em mar/2025 para não injetar mais nos jogos.
  - Ainda não pesquisado (as partes caíram no limite de sessão): os efeitos do Desktop Duplication no *independent flip*/MPO e a validação do *DWM shared surface*.
    Isso pode ser medido direto no Windows com PresentMon.
- **Pesquisas brutas** (JSON com todas as fontes): [`docs/pesquisa-bruta/`](pesquisa-bruta/).

## 6. Estado da implementação (em 08/10/2026)

### Feito e no GitHub

- `docs/`: pesquisa v2 + anexo + este arquivo.
- Workspace Cargo (`Cargo.toml`, `Cargo.lock`, `.gitignore`, `README.md`, `CLAUDE.md`).
- **Fase A CONCLUÍDA (08/10/2026).** Implementada por agentes e revisada adversarialmente: sonnet para proto, crypto e worker; opus para clock e buffer.
  - `crates/duoclip-proto`: mensagens (ClipRequest, ClipAck, ClipExtend, TimePing/Pong, RangeRequest, ChunkAvailable, ClipKey...),
    validação rígida (ids só em UUID minúsculo canônico), codec JSON com limite de 64 KiB e nomes das chaves no bucket.
  - `crates/duoclip-crypto`: `ClipKey` (zeroize), AES-256-GCM por bloco com AAD (`DCC1`), manifesto cifrado (`DCM1`) com checagens,
    e Chunker (o último bloco sempre sai marcado `is_last`).
  - `crates/duoclip-clock`: Relógio Global. Pacotes SNTP com pivô de 2036, estimador por fonte com **limite de erro rigoroso**
    (pior caso, ≥ 99% em simulação), Marzullo (descarta "falsetickers"), AppClock só com slew, epochs e holdover, remapeamento de
    dois lados, checagem cruzada P2P, cliente SNTP e agendador com KoD. **NTS ainda não está implementado** (só SNTP).
  - `crates/duoclip-buffer`: ring buffer de pacotes codificados + "fixar e coletar" (pós-roll), fragmentos por GOP, cobertura e lacunas,
    extensão, timeout, fim de fonte, gerador sintético e testes adversariais.
  - `worker/`: Cloudflare Worker (TypeScript) com auth Ed25519 e anti-replay, crews e convites, registro de clipes, URLs
    pré-assinadas do R2 que **assinam `Content-Length`**, cotas e disjuntores contra abuso, e varredura de hora em hora.
    Migrations D1 `0001` e `0002`. README em português com o passo a passo de deploy.
  - **Verificação final:** 244 testes Rust passando (1 ignorado: precisa de UDP 123), clippy `-D warnings` limpo, `cargo fmt` ok,
    `cargo check --target x86_64-pc-windows-gnu` ok; Worker: `tsc` ok e 312 testes passando.
  - Os desvios aceitos em relação aos SPECs estão no fim de cada `SPEC.md` ("Implementation notes"). **Leia antes de integrar.**
    Destaques: o cliente precisa enviar o PUT com o `content_length` exato e `manifest_size`; `AppClock::freeze()` devolve a linha-alvo;
    configurar só as faixas de áudio ativas no buffer.

- **Fase B1 PARCIAL (08/10/2026).** O workflow bateu no **limite de sessão** dos agentes. Situação:
  - `crates/duoclip-gamesdb` (sonnet): ✅ **implementado** (30 jogos, validação, `choose_backend`; 51 testes), mas **sem a revisão adversarial**.
    Os dados de anti-cheat batem com a pesquisa da rodada 3 (todos com `verified: false`).
  - `crates/duoclip-mux` (opus): 🟡 **implementação parcial, sem revisão.** Há código para Annex B, boxes, MP4 fragmentado, MP4 progressivo e timing,
    com testes estruturais e com ffmpeg passando (estes pulam se o ffmpeg não estiver instalado). O agente não terminou o relatório.
  - `crates/duoclip-audio` (opus): 🟡 **implementação parcial, sem revisão.** A parte portável (raízes do Discord, timestamps) tem testes.
    O módulo `wasapi/` (ativação, captura) só foi compilado e nunca executado. Falta conferir se mic, loopback do dispositivo e retry estão completos.
  - `crates/duoclip-encode` (opus): ❌ **não começou** (só esqueleto + SPEC).
  - Estado do workspace em 08/10 13h UTC: compila em Linux e Windows (gnu), e **362 testes passam**.
- Pesquisas brutas e relatórios dos agentes: [`docs/pesquisa-bruta/`](pesquisa-bruta/).
  Scripts dos workflows, para refazer ou continuar: [`tools/agent-workflows/`](../tools/agent-workflows/).

### Em andamento / pendente

**Comunicação direta e organização local (decisão do usuário em 08/10/2026):**

- Claude e GPT trocam entregas, revisões e respostas pelos arquivos, sem retransmissão do usuário.
  Protocolo em [`COMUNICACAO-AGENTES.md`](COMUNICACAO-AGENTES.md), branch `gpt/comunicacao-agentes`, tarefa 14.
  A caixa canônica fica sempre em `C:\Users\bolad\Projetos\duoclip\docs\comunicacao`, mesmo ao trabalhar em outra branch.
- A pasta principal continua em `C:\Users\bolad\Projetos\duoclip`; todos os worktrees foram reunidos em `worktrees/`:
  `gpt-worker-presenca`, `gpt-worker-r2`, `gpt-comunicacao` e `claude-duoclip-session`.
  Use essa pasta também para novos worktrees. As credenciais permanecem no arquivo ignorado `worker/.dev.vars` da pasta principal.
- Tarefa 10 entregue na branch `gpt/worker-presenca`, HEAD `48c0f8a`; código testado em `5573717`: 355 testes Worker e 374 Rust.
  Pedido de revisão ao Claude: `docs/comunicacao/para-claude/2026-10-08-gpt-001-worker-presenca.md` na pasta principal.
- Tarefa 13 entregue na branch `gpt/worker-r2`, HEAD `fddcf27`: 321 testes Worker e 374 Rust; R2 real validado pelo usuário
  com os status `404, 200, 200, 403, 204, 404` e limpeza concluída. O usuário consultou o D1 pelo Wrangler com sucesso:
  `duoclip`, ID `696b75a4-5499-402d-8076-d20c16f52ac7`, região `ENAM`, 0 tabelas. Migrações remotas e deploy pendentes.
  Pedido de revisão ao Claude: `docs/comunicacao/para-claude/2026-10-08-gpt-002-worker-r2.md`.
- A caixa usa um arquivo novo por mensagem e confirmações do próprio destinatário. Publicar um arquivo não acorda outra sessão;
  agentes ativos consultam a caixa antes, entre etapas e depois do trabalho. Os pedidos ao Claude ainda aguardam suas respostas.
- A configuração foi disponibilizada localmente na pasta principal antes da integração da branch de documentação para permitir leitura imediata.
  Preserve mensagens novas ao integrar. As branches Worker continuam separadas até a revisão cruzada.

1. **Terminar a B1:**
   - revisar o gamesdb;
   - terminar e revisar mux e áudio;
   - implementar e revisar o encode.

   Basta rodar de novo `tools/agent-workflows/duoclip-phase-b1-implement.js`. Os agentes encontram o código parcial e continuam.
2. **Fase B2:** `duoclip-capture` com os backends `dda_crop` (padrão no Win10) e `wgc` (Win11, com detecção da API e sem borda), o stub do hook,
   as práticas de desempenho da seção 4.6 do documento de pesquisa e o tratamento de foco/oclusão no estilo Medal.
3. **Teste em Windows real:** o prompt para o Claude Code local está em [`docs/PROMPT-CLAUDE-CODE-LOCAL.md`](PROMPT-CLAUDE-CODE-LOCAL.md).

### Próximos passos (roadmap)

| Fase | O que fazer |
|---|---|
| **A** ✅ concluída | proto, crypto, worker, clock e buffer implementados, revisados e testados |
| **B1** 🟢 implementada, em revisão pelo GPT | encode ✅ (validado com NVENC) · áudio ✅ (limiar de 2 ms) · mux ✅ · gamesdb ✅ — branch `claude/fase-b1`, ver `docs/TAREFAS.md` |
| **B2** | `duoclip-capture` (dda_crop + wgc + stub do hook), bucket local fMP4 integrado, **benchmark PresentMon comparando com o Medal** e teste da borda no Win11 |
| **C** | Rede: WebRTC + Worker (signaling) + upload e download cifrados no R2. Integração com o relógio global e o protocolo. **Sessão automática por grupo, até 8 pessoas** (4.7). |
| **D** | App **Tauri 2**: bandeja, tecla de clipe, amigos e **vários grupos**, escolha de grupo quando houver conflito, indicador "±X ms", editor com **até 8 POVs** sincronizados e exportação |
| **E** | Banco de jogos remoto, testes com anti-cheats (Vanguard, EAC, BattlEye, FACEIT, Gamers Club), instalador e atualização |
| **F (futuro, fora do MVP)** | Modo **hook opcional** (`duoclip-hook`), só para o Minecraft Java no começo, depois de medir |

> **Ainda não existe um app executável.** Por enquanto há bibliotecas testadas e o Worker. O primeiro executável de teste no Windows serão as
> ferramentas de diagnóstico (smoke tests) de áudio, encoder e captura, criadas pelo prompt do Claude Code local.

### Validações pendentes (para fazer em máquinas Windows reais)

Primeiro teste real em 08/10/2026 (Win11 26300, RTX 5060 Ti): [`docs/relatorios/teste-local-2026-10-08.md`](relatorios/teste-local-2026-10-08.md).
Os diagnósticos ficam no crate `crates/duoclip-smoke` (`sysinfo`, `audio_probe`, `encoder_probe`, `capture_probe`).

1. ✅ **Compilação nativa com MSVC** (08/10, Rust 1.99, VS Build Tools 2022): nenhum erro. Só um lint novo do clippy (`chunks_exact_to_as_chunks`), já corrigido.
   374 testes passando (com o smoke), clippy limpo, worker com 312 testes.
2. 🟡 **Process loopback:** ✅ Discord pelo processo raiz no **Win11 26300**, 3/3 ativações sem retry, depois de corrigir um **double free** (`Drop for PROPVARIANT`
   do crate `windows` → `STATUS_HEAP_CORRUPTION` em todo início de process loopback). Microfone e endpoint loopback ok.
   Achados: os timestamps do process loopback são sintéticos (passos exatos de 10 ms) e às vezes saltam ~8–9 ms sem flag (abaixo do limiar de 20 ms do tracker);
   não usa a flag `SILENT`. ❌ Falta: **Win10 19045**, e o process loopback do **jogo** (`audio_probe --capture --game-exe x.exe`).
3. 🟡 **Encoders MF:** ✅ NVIDIA (RTX 5060 Ti): H.264, HEVC e AV1 de hardware ativam. O MFT H.264 de hardware exige **NV12**
   (validado com ffmpeg `h264_mf`; `h264_nvenc` 1080p60 ok). ❌ Falta: AMD e Intel, e o teste com o `duoclip-encode` (ainda é esqueleto).
4. ❌ Benchmark PresentMon: Desktop Duplication recortado × WGC × Medal ligado (CS2, Valorant, Fortnite, LoL, Minecraft, Roblox).
   Medir também se o Desktop Duplication tira o jogo do *independent flip*. (PresentMon e Medal não estavam instalados; o `capture_probe` está pronto mas
   ainda não rodou: o usuário pulou a captura de tela.) A máquina de teste tem **HDR no monitor principal e um monitor girado 90°**, e o DDA precisa tratar os dois.
5. ❌ Borda do WGC no Windows 11: sem pacote, com a configuração do Windows ligada, e com MSIX/pacote esparso. (No 26300, `IsBorderRequired` e `GraphicsCaptureAccess` existem.)
6. ❌ "Teste do flash" do relógio global entre dois PCs (meta ≤ 1 frame). (O teste SNTP contra `time.cloudflare.com` passou no Windows.)
7. 🟡 Cloudflare: ✅ presigner contra R2 real validado pelo usuário, com limpeza concluída; ✅ consulta do D1 pelo Wrangler.
   ❌ migrações D1 e Worker publicado ainda pendentes (relatório em `gpt/worker-r2:worker/R2-VALIDACAO.md`).
8. Opcional: teste "caixa-preta" do Medal 2026 (`tasklist /m medal-hook64.dll` e os logs em `%AppData%\Medal`).

## 7. Como retomar em outro ambiente

```bash
git clone https://github.com/nicolaspercio1/duoclip.git && cd duoclip
git checkout claude/sync-gameplay-clip-app-xpgfwx

# Rust estável (usamos 1.97). No Windows, use o toolchain MSVC padrão.
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
# No Linux/macOS, checagem cruzada para Windows:
rustup target add x86_64-pc-windows-gnu && cargo check --workspace --target x86_64-pc-windows-gnu

# Worker (Node 22)
cd worker && npm install && npm run typecheck && npm test
```

- As Fases B a E precisam de **Windows** para rodar e testar (captura, áudio e encoder). Instale Visual Studio Build Tools, Rust (MSVC),
  FFmpeg (build LGPL com NVENC, AMF e QSV), PresentMon e Node 22.
- Num ambiente com Claude Code, o arquivo `CLAUDE.md` na raiz é carregado automaticamente e aponta para este arquivo.
- Para baixar tudo e testar no seu PC com o Claude Code no terminal, use o prompt pronto em [`docs/PROMPT-CLAUDE-CODE-LOCAL.md`](PROMPT-CLAUDE-CODE-LOCAL.md).
- O FFmpeg (com libx264 e aac) é usado nos testes do mux. Sem ele, esses testes são pulados. No Windows: `winget install Gyan.FFmpeg`.
