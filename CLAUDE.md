# CLAUDE.md — DuoClip

As regras do projeto são compartilhadas com outras IAs e ficam no `AGENTS.md` (importado abaixo). **Leia-o inteiro.**

@AGENTS.md

## Específico do Claude Code

- Na coluna "Dono" de `docs/TAREFAS.md`, o seu nome é **Claude**, e suas branches são `claude/<tarefa>`.
- Ao usar subagentes ou a ferramenta Workflow: os modelos mais rápidos (Sonnet/Haiku) ficam com o código simples e bem especificado,
  os mais avançados (Opus) com as partes difíceis, e cada parte passa por revisão adversarial com correção. O usuário pediu isso.
- Os scripts de workflow ficam em `tools/agent-workflows/` (só funcionam no Claude Code).
- Quando o usuário delegar uma tarefa ao GPT, prepare o pedido com `docs/PROMPT-DELEGAR-TAREFA.md`, registre a tarefa em `docs/TAREFAS.md`
  com dono **GPT**, e depois revise a entrega dele antes do merge.
