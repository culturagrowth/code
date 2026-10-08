# Revisão — tarefa nº 15: protocolo, segunda rodada

- Branch: `gpt/protocolo-2` · commits revisados: `b11cdde..f4108f121b542d962d5f7354762dbb03736f857f`
- Implementado por: GPT · Revisado por: Claude · Data: 2026-10-08
- Checagens: só documentação (5 arquivos: `AGENTS.md`, `docs/COMUNICACAO-AGENTES.md`, `docs/MEMORIA-DO-PROJETO.md`,
  `docs/PROMPT-DELEGAR-TAREFA.md`, `docs/revisoes/README.md`); não há código. Revisão feita em `worktrees/claude-revisao-protocolo-2`,
  criado no SHA exato, como o próprio protocolo novo pede.

## Conferência dos achados da tarefa 14

1. **Resolvido** — pasta principal fixa na integração, com o motivo (a caixa versionada some ao trocar de branch) e o que fazer se ela
   for encontrada noutra branch (não concluir que a caixa está vazia, preservar e avisar). Está no `AGENTS.md` e no protocolo.
2. **Resolvido** — revisão lendo pelo repositório principal e checagens em worktree próprio, com o exemplo de comandos;
   proibição explícita de mexer em `safe.directory`, permissões ou no worktree alheio.
3. **Resolvido** — relatório na branch `<revisor>/revisao-<tarefa>`; o autor responde pela caixa e o revisor incorpora após conferir;
   o relatório entra na integração como documentação separada. Isso é coerente com o que fiz nas tarefas 10 e 13
   (integrei as branches de revisão, que trazem o código do autor e o commit do relatório: "procedimento equivalente").
4. **Resolvido** — memória só com decisões e links; regra geral nova no item 6 do `AGENTS.md`.
5. **Resolvido** — sequência por remetente e por dia, a partir de 001, considerando as duas pastas, sem sobrescrever.

Bom acréscimo: a exceção explícita para commits pequenos de metadados (quadro e mensagens próprias) na integração, com a proibição de
incluir código, regras ou arquivos do outro agente nesses commits. É exatamente a prática que estamos usando.

## Achados

### 1. [menor] Caminho do relatório do R2 na memória
- Onde: `docs/MEMORIA-DO-PROJETO.md`, item 7 das validações: "evidências do R2 em `gpt/worker-r2:worker/R2-VALIDACAO.md`"
- Problema: a tarefa 13 foi integrada depois desta entrega (merge `93fcffa`); o arquivo agora está em `worker/R2-VALIDACAO.md` na integração.
- Sugestão: resolvo no próprio merge (é uma linha de memória e o conflito com a integração já obriga a mexer nesse bloco).

## Veredito
**Aprovado.** Integro na branch de integração, preservando no bloco da memória a decisão da presença (30 s/90 s) que entrou com a tarefa 10.
