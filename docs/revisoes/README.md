# Revisões cruzadas

Regra do projeto ([`AGENTS.md`](../../AGENTS.md), item 5): **quem implementa não revisa.** O GPT revisa o Claude e o Claude revisa o GPT.
Cada revisão é um arquivo `<tarefa>.md` nesta pasta, criado numa branch **do revisor**:
`gpt/revisao-<tarefa>` ou `claude/revisao-<tarefa>`, a partir do SHA exato entregue.
As checagens rodam num worktree próprio em `worktrees/<revisor>-revisao-<tarefa>`.
O implementador mantém sua branch e pasta; o revisor não as modifica.
Para código já integrado, use o SHA da integração que será auditado como ponto de partida.

## Formato

```markdown
# Revisão — tarefa nº <n>: <título>

- Branch: `<branch>` · commits revisados: `<sha>..<sha>`
- Implementado por: <Claude|GPT> · Revisado por: <GPT|Claude> · Data: AAAA-MM-DD
- Checagens: fmt <ok/falhou> · clippy <ok/falhou> · testes <N passaram / M falharam> · não verificado: <o que não deu para rodar>

## Achados

### 1. [crítico|importante|menor] <título curto>
- Onde: `caminho/arquivo.rs:linha`
- Problema: <o que está errado>
- Cenário que falha: <entrada/estado concreto → resultado errado>
- Sugestão: <como corrigir>
- Resposta do autor: <corrigido em <sha> | discordo porque ... | fica para depois (tarefa nº ...)>

## Veredito
<aprovado | aprovado com ressalvas | mudanças necessárias> — <uma frase>
```

O autor lê o relatório com `git show <branch-ou-commit>:docs/revisoes/<tarefa>.md` e responde pela caixa compartilhada,
referenciando os IDs dos achados e os commits de correção na própria branch de implementação.
Depois das correções, o revisor confere, registra as respostas no seu arquivo e atualiza o veredito.
Publique o resultado na caixa com SHA revisado, commit do relatório, caminho e veredito.
Na integração da tarefa aprovada, inclua o commit do relatório como documentação separada, preservando a autoria.
