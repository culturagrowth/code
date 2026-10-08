# AGENTS.md — DuoClip (instruções para qualquer IA)

Este arquivo vale para **qualquer assistente de código** que trabalhe neste repositório: Claude (Claude Code), GPT (Codex ou outro) e
os que vierem depois. O `CLAUDE.md` importa este arquivo; o que for específico de uma ferramenta fica no arquivo dela.

App Windows para amigos que grava o jogo e, num atalho, salva o POV de todos os amigos da sessão, sincronizado por um relógio
global (com segundos antes e depois do aperto), trocando os clipes cifrados por um bucket Cloudflare R2.

**Leia primeiro:**
1. [`docs/MEMORIA-DO-PROJETO.md`](docs/MEMORIA-DO-PROJETO.md): decisões, histórico, estado atual e próximos passos.
2. [`docs/TAREFAS.md`](docs/TAREFAS.md): quem está fazendo o quê agora. **Pegue só tarefas livres ou atribuídas a você.**
3. O `SPEC.md` de cada crate em que for mexer.

Arquitetura completa: [`docs/pesquisa-app-clipes-sincronizados.md`](docs/pesquisa-app-clipes-sincronizados.md).
Pesquisas brutas com fontes: `docs/pesquisa-bruta/`. Relatórios de testes reais: `docs/relatorios/`.

## Regras de trabalho

- Responder ao usuário em **português do Brasil**, direto ao ponto. Se errar, admitir e corrigir.
- **Nunca inventar fatos técnicos** (APIs, flags, comportamento do Windows). Verifique no código-fonte, na documentação ou com um teste real.
  Se for suposição, diga que é suposição.
- Código, identificadores e comentários em inglês. Documentação para o usuário em português.
- Cada crate tem um `SPEC.md` como contrato. Mudou a API? Atualize o SPEC junto. Desvios vão em "Implementation notes" no fim do SPEC.
- Crates independentes de plataforma usam `#![forbid(unsafe_code)]` e nunca entram em pânico com entrada não confiável.
  Código Windows fica atrás de `#[cfg(windows)]`, com `unsafe` mínimo e um comentário `// SAFETY:` em cada bloco. Confira cada HRESULT.
- Crate `windows` 0.62: consulte o código-fonte em `~/.cargo/registry/src/*/windows-0.62.2` em vez de adivinhar a API.
  Armadilhas já encontradas: ele implementa `Drop for PROPVARIANT` (chama `PropVariantClear`), e métodos que devolvem `Result` tratam `S_FALSE` como `Ok`.
- Saídas de testes manuais (WAV, PNG, MP4) vão para `test-output/`, que nunca é commitada.
- Testes que dependem de hardware (GPU, áudio) ficam com `#[ignore = "needs ..."]` e são rodados à parte no Windows.
- Antes de concluir qualquer mudança de código, tudo tem que passar:
  ```bash
  cargo fmt --all
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  cargo check --workspace --all-targets --target x86_64-pc-windows-gnu
  cd worker && npm run typecheck && npm test   # se mexeu no worker
  ```
- Mantenha `docs/MEMORIA-DO-PROJETO.md` atualizado a cada decisão importante, e o `docs/TAREFAS.md` a cada tarefa pega ou concluída.

## Segurança e limites (valem para todas as IAs)

- Nunca rodar nada como administrador, nunca injetar nada em jogos, nunca alterar configurações do Windows.
- Não instalar nada sem mostrar o comando ao usuário e esperar a confirmação.
- **Antes de gravar tela, áudio do sistema, áudio de outros processos ou microfone, avise o usuário e espere a confirmação.**
  Testes automáticos usam só dados sintéticos e o loopback do próprio processo de teste.
- Nunca commitar segredos (chaves, tokens). Credenciais ficam fora do repositório.

## Trabalho com mais de uma IA

O usuário divide tarefas entre o Claude e o GPT. Para não haver conflito:

1. **Uma tarefa por vez, com dono.** Antes de começar, a tarefa tem que estar em `docs/TAREFAS.md` com o seu nome (`Claude` ou `GPT`)
   na coluna "Dono". Se estiver com outro dono, não mexa.
2. **Uma branch por tarefa**, criada a partir da branch de integração `claude/sync-gameplay-clip-app-xpgfwx`:
   `claude/<tarefa>` ou `gpt/<tarefa>` (ex.: `gpt/worker-presenca`). Nunca faça commit direto na branch de outra IA.
3. **Escopo por pasta.** Cada tarefa diz quais pastas pode alterar (normalmente um crate ou o `worker/`). Fora delas, só leitura.
   Mudanças em arquivos compartilhados (`Cargo.toml` da raiz, `Cargo.lock`, `AGENTS.md`, `docs/MEMORIA-DO-PROJETO.md`) ficam no fim da
   tarefa, em commit separado, para facilitar o merge.
4. **Entrega:** checagens passando, `docs/TAREFAS.md` atualizado **na branch de integração** (status `em revisão`, branch, resumo), notas no SPEC e, se houve decisão,
   na memória. Mensagens de commit em português. A branch fica esperando a revisão da outra IA.
5. **Revisão cruzada obrigatória (decisão do usuário):** **quem implementa não revisa o próprio trabalho.** O GPT revisa o que o Claude
   fez, e o Claude revisa o que o GPT fez. Não use subagentes da mesma IA como "revisão": isso não substitui a revisão da outra.
   - O revisor lê o SPEC e o diff da branch (`git diff <integração>...<branch>`), roda todas as checagens e procura bugs de verdade
     (correção, segurança, `unsafe`, pânico com entrada não confiável, desvios do SPEC, testes fracos).
   - O resultado vai em `docs/revisoes/<tarefa>.md`: cada achado com gravidade (crítico, importante, menor), arquivo e linha, cenário
     que falha e sugestão. No fim, um veredito: **aprovado**, **aprovado com ressalvas** ou **mudanças necessárias**.
   - O revisor **não corrige** o código da outra IA (só em ajustes triviais, se o dono pedir). Quem implementou corrige na própria branch e
     responde cada achado no mesmo arquivo de revisão.
   - Com o veredito "aprovado", o merge na branch de integração é feito pelo Claude (que roda no PC do usuário), ou por quem o usuário indicar.
     O que depende de hardware (GPU, áudio, captura) e que o revisor não puder executar é marcado como "não verificado" na revisão.

## Decisões que não devem ser revertidas sem falar com o usuário

- **Captura sem injeção por padrão:** Desktop Duplication recortado no Windows 10 (sem borda amarela); WGC sem borda ou o mesmo DDA no Windows 11.
  O **hook estilo Medal** é só um modo opcional futuro, **fora do MVP**, apenas para jogos sem anti-cheat. Pela pesquisa, no começo
  é só o Minecraft Java. Há uma lista de bloqueio fixa, e o hook se desliga se um anti-cheat de kernel estiver rodando.
  No Windows 10 **não existe forma documentada de tirar a borda do WGC**, então nunca use WGC como padrão lá.
- **Windows 10 22H2 e Windows 11** suportados por completo. **Nada de borda amarela.** A eficiência tem que ficar no nível do Medal.
- **Relógio Global DuoClip:** QPC disciplinado para UTC (NTP.br + Cloudflare, NTS depois) com refino P2P. Nunca usar o relógio do Windows.
- **Pós-roll "fixar e coletar":** 30 s antes, 10 s depois e ±2 s de margem.
- **Bucket na nuvem = Cloudflare R2**, com criptografia ponta a ponta, expiração ≤ 72 h e URLs assinadas pelo Worker.
- Uso **privado entre amigos:** sem tela de consentimento e sem exigências jurídicas extras.
- **Grupos, não dupla:** cada pessoa pode estar em vários grupos isolados. A **sessão é automática** (membros do mesmo grupo, com o app aberto
  e no mesmo jogo; pergunta quando houver dois grupos online) e suporta **até 8 pessoas**. O clipe vai só para quem está na sessão.

## Ambiente do PC do usuário (Windows 11, para quem roda localmente)

- Repositório em `C:\Users\bolad\Projetos\duoclip`; remoto `https://github.com/nicolaspercio1/duoclip`.
- Rust 1.99 MSVC + Visual Studio Build Tools 2022. O `cargo` fica em `~/.cargo/bin` e pode não estar no PATH: no Git Bash, use
  `export PATH="$HOME/.cargo/bin:$PATH"`.
- Node 24, ffmpeg/ffprobe 8.1 (com libx264 e aac). GPU NVIDIA RTX 5060 Ti (NVENC), monitor principal com HDR e um monitor girado 90°.
