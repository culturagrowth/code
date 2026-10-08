# CLAUDE.md — DuoClip

As regras do projeto são compartilhadas com outras IAs e ficam no `AGENTS.md` (importado abaixo). **Leia-o inteiro.**

@AGENTS.md

## Específico do Claude Code

- Na coluna "Dono" de `docs/TAREFAS.md`, o seu nome é **Claude**, e suas branches são `claude/<tarefa>`.
- Ao usar subagentes ou a ferramenta Workflow para **implementar**: os modelos mais rápidos (Sonnet/Haiku) ficam com o código simples e
  bem especificado, os mais avançados (Opus) com as partes difíceis.
- **Revisão é do GPT, não de subagentes do Claude** (decisão do usuário em 08/10/2026, ver `AGENTS.md`, item 5). Ao terminar uma tarefa,
  deixe-a na branch `claude/<tarefa>` com status `em revisão` e prepare o pedido de revisão com `docs/PROMPT-DELEGAR-TAREFA.md`.
  Quando o GPT entregar uma tarefa, você é o revisor: escreva `docs/revisoes/<tarefa>.md` e não corrija o código dele.
- Os scripts de workflow ficam em `tools/agent-workflows/` (só funcionam no Claude Code).
- Quando o usuário delegar uma tarefa ao GPT, prepare o pedido com `docs/PROMPT-DELEGAR-TAREFA.md` e registre a tarefa em `docs/TAREFAS.md`
  com dono **GPT** e revisor **Claude**.
