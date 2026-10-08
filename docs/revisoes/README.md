# Revisões cruzadas

Regra do projeto ([`AGENTS.md`](../../AGENTS.md), item 5): **quem implementa não revisa.** O GPT revisa o Claude e o Claude revisa o GPT.
Cada revisão é um arquivo `<tarefa>.md` nesta pasta, criado pelo revisor na branch revisada.

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

O autor responde cada achado no próprio arquivo. Depois das correções, o revisor confere e atualiza o veredito.
