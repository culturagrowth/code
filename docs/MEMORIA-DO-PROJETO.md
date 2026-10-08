# Memória do projeto DuoClip

> Arquivo de memória para migrar o projeto para outro ambiente sem perder contexto.
> Resume **tudo o que foi pedido, pesquisado, decidido e implementado** na conversa de 07–08/10/2026
> (sessão Claude Code na nuvem). Mantenha este arquivo atualizado a cada decisão importante.
>
> - Repositório: `culturagrowth/code` · branch de trabalho: `claude/sync-gameplay-clip-app-xpgfwx`
> - Idioma de trabalho: **português do Brasil**. O código e os comentários ficam em inglês.
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
  - **Em avaliação:** *DWM shared surface* (`DwmGetDxSharedSurface`, API não documentada usada pelo Magpie), também sem borda.
- **Opcional e futuro (Fase 6): hook injetado no estilo Medal.** Desligado por padrão e ligado jogo a jogo, **só em jogos sem anti-cheat**
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

## 6. Estado da implementação (em 08/10/2026)

### Feito e no GitHub

- `docs/`: pesquisa v2 + anexo + este arquivo.
- Workspace Cargo (`Cargo.toml`, `Cargo.lock`, `.gitignore`, `README.md`), com os crates:
  - `crates/duoclip-proto`: protocolo de mensagens, validação e nomes das chaves (SPEC pronto);
  - `crates/duoclip-clock`: relógio global (SPEC pronto, o mais detalhado);
  - `crates/duoclip-buffer`: ring buffer + pós-roll "fixar e coletar" (SPEC pronto);
  - `crates/duoclip-crypto`: criptografia ponta a ponta + chunker (SPEC pronto);
  - `worker/SPEC.md`: Cloudflare Worker (SPEC pronto).

### Em andamento quando este arquivo foi escrito

- **Workflow "Fase A"** (agentes): implementar `duoclip-proto` + `duoclip-crypto` (sonnet), `worker/` (sonnet), `duoclip-clock` (opus)
  e `duoclip-buffer` (opus), cada um com revisão adversarial e correção.
  **Se o código desses crates ainda estiver só com stubs no GitHub, a Fase A não terminou:** rode de novo seguindo os `SPEC.md`.
- **Workflow de pesquisa de captura sem borda no Windows 10:** compara Desktop Duplication recortado × DWM shared surface × hook,
  com uma tabela de anti-cheat por jogo popular no Brasil. Quando terminar, atualizar a seção 4 do documento de pesquisa.

### Próximos passos (roadmap)

| Fase | O que fazer |
|---|---|
| **A** (em andamento) | proto, crypto, worker, clock e buffer implementados e testados no Linux. Checagem cruzada `cargo check --target x86_64-pc-windows-gnu`. |
| **B** (precisa de Windows para testar) | `duoclip-capture` (trait `CaptureBackend`, backend **dda_crop** e backend **wgc**; hook só como stub), `duoclip-audio` (WASAPI process loopback: jogo + Discord + mic), `duoclip-encode` (FFmpeg/NVENC/AMF/QSV com textura D3D11), escritor de **bucket local fMP4**, teste da borda no Win11 e **benchmark PresentMon comparando com o Medal** |
| **C** | Rede: WebRTC + Worker (signaling) + upload e download cifrados no R2. Integração com o relógio global e o protocolo. |
| **D** | App **Tauri 2**: bandeja, tecla de clipe, amigos e crews, indicador "±X ms", editor com POVs sincronizados e exportação |
| **E** | Banco de jogos, testes com anti-cheats (Vanguard, EAC, BattlEye, FACEIT, Gamers Club), instalador e atualização |
| **F (futuro)** | Modo **hook opcional** (`duoclip-hook`), só em jogos sem anti-cheat |

### Validações pendentes (para fazer em máquinas Windows reais)

1. Benchmark PresentMon: Desktop Duplication recortado × WGC × Medal ligado (CS2, Valorant, Fortnite, LoL, Minecraft, Roblox).
2. Borda do WGC no Windows 11: sem pacote, com a configuração do Windows ligada, e com MSIX/pacote esparso.
3. Process loopback do Discord pelo processo raiz, no Windows 10 19045 e no Windows 11.
4. "Teste do flash" do relógio global entre dois PCs (meta ≤ 1 frame).
5. Opcional: teste "caixa-preta" do Medal 2026 (`tasklist /m medal-hook64.dll` e os logs em `%AppData%\Medal`) para confirmar o padrão atual.
6. Política de uso do NTP.br/NIC.br para embutir num app, se um dia o app for público.

## 7. Como retomar em outro ambiente

```bash
git clone https://github.com/culturagrowth/code.git && cd code
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
