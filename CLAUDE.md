# CLAUDE.md — DuoClip

App Windows para amigos que grava o jogo e, num atalho, salva o POV de todos, sincronizado por um relógio
global (com segundos antes e depois do aperto), trocando os clipes cifrados por um bucket Cloudflare R2.

**Leia primeiro:** [`docs/MEMORIA-DO-PROJETO.md`](docs/MEMORIA-DO-PROJETO.md). Lá estão todas as decisões, o histórico, o estado atual e os próximos passos.
Pesquisas brutas com fontes: `docs/pesquisa-bruta/`. Scripts dos agentes: `tools/agent-workflows/`. Teste local: `docs/PROMPT-CLAUDE-CODE-LOCAL.md`.
Arquitetura completa: [`docs/pesquisa-app-clipes-sincronizados.md`](docs/pesquisa-app-clipes-sincronizados.md).

## Regras de trabalho

- Responder ao usuário em **português do Brasil**, direto ao ponto. Se errar, admitir e corrigir.
- Código, identificadores e comentários em inglês. Documentação para o usuário em português.
- Cada crate tem um `SPEC.md` como contrato. Mudou a API? Atualize o SPEC junto.
- Crates independentes de plataforma usam `#![forbid(unsafe_code)]` e nunca entram em pânico com entrada não confiável.
  Código Windows fica atrás de `#[cfg(windows)]`, com `unsafe` mínimo e um comentário `// SAFETY:` em cada bloco.
- Saídas de testes manuais (WAV, PNG, MP4) vão para `test-output/`, que nunca é commitada.
- Antes de concluir: `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`,
  `cargo check --workspace --target x86_64-pc-windows-gnu`. No worker: `npm run typecheck && npm test`.
- Ao usar agentes: os modelos mais rápidos (sonnet) ficam com o código simples, os mais avançados (opus) com as partes difíceis,
  e cada parte passa por revisão adversarial com correção. O usuário pediu isso.
- Mantenha `docs/MEMORIA-DO-PROJETO.md` atualizado a cada decisão importante.

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
