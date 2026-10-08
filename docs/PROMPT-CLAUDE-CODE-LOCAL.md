# Prompt para o Claude Code local (CLI): baixar tudo e testar o DuoClip

Copie o bloco abaixo e cole no Claude Code aberto no terminal do seu PC (de preferência **Windows 10 22H2 ou 11**,
porque é lá que captura, áudio e encoder rodam de verdade). Rode o Claude Code numa pasta onde ele possa clonar o repositório.

> **O que ainda não existe:** um app executável completo. Hoje há bibliotecas testadas (relógio global, buffer com pós-roll,
> criptografia, protocolo, banco de jogos, MP4 e áudio parcial) e o Worker. Este prompt **testa tudo o que já existe** e cria
> **ferramentas de diagnóstico** para validar no seu Windows as partes que só funcionam lá (áudio do Discord e do jogo,
> encoders da GPU e Desktop Duplication).

````text
Você vai baixar e testar o projeto DuoClip no meu PC. Responda sempre em português do Brasil.

## 0. Contexto
- Repositório: https://github.com/nicolaspercio1/duoclip — branch: claude/sync-gameplay-clip-app-xpgfwx
- Antes de qualquer coisa, leia o CLAUDE.md da raiz e o docs/MEMORIA-DO-PROJETO.md, porque eles têm todas as decisões e o estado atual.
  Leia também os SPEC.md dos crates que for testar.
- Regras que não podem ser quebradas:
  - nunca rodar nada como administrador;
  - nunca injetar nada em jogos;
  - não alterar configurações do Windows;
  - não instalar nada sem me mostrar o comando e pedir confirmação;
  - antes de gravar tela, áudio ou microfone, me avise e espere eu confirmar.

## 1. Obter o código
- Se a pasta `code` não existir: `git clone https://github.com/nicolaspercio1/duoclip.git` e `git checkout claude/sync-gameplay-clip-app-xpgfwx`.
- Se já existir: `git fetch origin` e `git checkout claude/sync-gameplay-clip-app-xpgfwx`, depois `git pull --rebase`.
  Se houver mudanças locais, me pergunte antes de mexer nelas.

## 2. Diagnóstico do ambiente (só leitura)
- Sistema operacional, versão e **build do Windows**: 19045 = Win10 22H2; ≥ 22000 = Win11.
- GPU(s), com fabricante, modelo e driver (`Get-CimInstance Win32_VideoController`); RAM; quantidade de monitores.
- Ferramentas:
  - git;
  - rustup/cargo (no Windows, toolchain **MSVC**, com o Visual Studio Build Tools e a carga "Desenvolvimento para desktop com C++");
  - Node ≥ 22.13;
  - ffmpeg/ffprobe (com libx264 e aac).
- Se faltar algo, me mostre os comandos de instalação (winget/rustup) e espere minha confirmação. Exemplos:
  - `winget install Rustlang.Rustup`;
  - `winget install Microsoft.VisualStudio.2022.BuildTools` (com a carga C++);
  - `winget install OpenJS.NodeJS.LTS`;
  - `winget install Gyan.FFmpeg`.

## 3. Testes automáticos (todos; anote a saída e a contagem de testes de cada um)
1. `cargo fmt --all -- --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace`
   - **No Windows, esta é a primeira compilação nativa com MSVC do código Windows** (até agora só houve checagem cruzada com o alvo gnu).
     Registre qualquer erro de compilação com o trecho completo.
4. Fora do Windows: `rustup target add x86_64-pc-windows-gnu` e `cargo check --workspace --all-targets --target x86_64-pc-windows-gnu`
5. `cd worker && npm ci && npm run typecheck && npm test`

Se houver erro simples (compilação no MSVC, teste quebrado, clippy), você pode corrigir seguindo o CLAUDE.md, rodando de novo TODAS as
checagens, e registrar a correção no relatório. Se o problema for grande ou de design, só reporte.

## 4. Testes reais no Windows (só se o sistema for Windows)
Crie um crate de diagnóstico `crates/duoclip-smoke`, com binários. Ele precisa compilar em qualquer sistema: fora do Windows, cada binário só
imprime "somente Windows". As saídas (WAV, PNG, MP4, CSV) vão para a pasta `test-output/`, que deve ficar no .gitignore e **nunca** ser commitada.
Use os crates existentes sempre que possível e escreva código cuidadoso (HRESULTs checados, `unsafe` mínimo com comentário `// SAFETY:`).

a) `sysinfo`:
   - build do Windows;
   - adaptadores DXGI (nome, fabricante, VRAM, LUID) e saídas/monitores (resolução, taxa, HDR);
   - se a API `GraphicsCaptureSession.IsBorderRequired` existe (`ApiInformation.IsPropertyPresent`; no Win10 deve dar "não");
   - estado do HAGS, se for possível ler sem admin.

b) `audio_probe` (usa `duoclip-audio`):
   - lista as raízes do Discord (`process_snapshot` + `discord_roots`);
   - com a minha confirmação, captura 10 s do **Discord** por process loopback. Vou estar numa call ou tocando um som no Discord;
   - com `--game-exe <nome.exe>`, captura também o jogo; com `--mic`, captura o microfone;
   - salva WAV float32 de cada fonte + CSV com os timestamps (QPC) de cada pacote;
   - relata: pacotes recebidos, % de silêncio, descontinuidades, timestamps extrapolados, e a diferença entre o tempo QPC e o tempo pelas
     amostras (drift, em ppm);
   - se a ativação falhar, relate o HRESULT. No Win10 19045 há falhas intermitentes conhecidas, então teste 3 vezes.

c) `encoder_probe`:
   - enumera os encoders H.264 de **hardware** e de software pelo Media Foundation (MFTEnumEx), com o nome de cada MFT, e o encoder AAC;
   - se o `crates/duoclip-encode` já estiver implementado (veja a memória e o SPEC): cria o device D3D11, codifica 3 s de quadros sintéticos
     1080p60, empacota com o `duoclip-mux` num .mp4 e valida com `ffprobe` (codec, frames, duração).

d) `capture_probe` (Desktop Duplication, o método padrão do DuoClip no Windows 10, sem borda amarela):
   - DuplicateOutput no monitor do jogo (ou o principal);
   - captura por 10 s;
   - mede quadros por segundo entregues pelo AcquireNextFrame e o tempo médio do recorte na GPU (`CopySubresourceRegion` para o retângulo da janela);
   - salva um PNG do recorte de uma janela que eu escolher (`--window "<título>"`);
   - confirme que **não aparece borda amarela**.

e) Opcional, se eu tiver o PresentMon instalado: com um jogo aberto, compare o FPS médio, o 1% low e o modo de apresentação com e sem o
   `capture_probe` rodando. Se eu tiver o Medal, compare também com ele ligado. Pergunte antes de rodar.

## 5. Relatório e memória
- Escreva `docs/relatorios/teste-local-AAAA-MM-DD.md`, em português, com:
  - o ambiente;
  - o resultado de cada comando (passou/falhou e a contagem de testes);
  - os erros do MSVC com trechos;
  - os números de cada probe;
  - os problemas encontrados;
  - as correções feitas;
  - as recomendações.
- Atualize a seção "Validações pendentes" do `docs/MEMORIA-DO-PROJETO.md`, marcando o que foi validado e com qual resultado.

## 6. Entregar
- Antes de commitar: `git pull --rebase`. Depois rode de novo as checagens do item 3, se tiver mudado código.
- Commit na mesma branch, com a mensagem em português, e `git push`. Nunca commite `test-output/` nem arquivos gravados.
- No fim, me mostre um resumo curto: o que passou, o que falhou, o que você corrigiu e o que precisa de mim.
````

## Depois do teste

- Se tudo passar, o próximo passo é terminar a Fase B1 (encode, revisões) e começar a B2 (captura). Os scripts dos agentes estão em
  `tools/agent-workflows/`.
- O relatório gerado em `docs/relatorios/` também alimenta a próxima sessão, porque fica no GitHub.
