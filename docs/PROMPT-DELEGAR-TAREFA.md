# Como delegar uma tarefa para outra IA (GPT ou outra)

1. Escolha uma tarefa **livre** em [`TAREFAS.md`](TAREFAS.md) (ou peça ao Claude para criar uma, com SPEC).
2. Mude o "Dono" para `GPT` e o status para `em andamento` (ou peça ao Claude para fazer isso), e faça o push.
3. Preencha o modelo abaixo e mande para o GPT:
   - **Com acesso à pasta do projeto** (ex.: um agente de terminal): ele lê o `AGENTS.md` sozinho, então basta a parte "Tarefa".
   - **Sem acesso** (chat): mande o modelo inteiro. Cole também o `SPEC.md` da tarefa e os arquivos que ele precisar ler.
4. Quando ele entregar, peça ao Claude: *"revise a entrega do GPT na branch `gpt/<tarefa>` e faça o merge se estiver ok"*.

## Modelo

````text
Você vai trabalhar no projeto DuoClip (Rust + Cloudflare Worker em TypeScript). Responda em português do Brasil.

## Regras do projeto
Leia e siga o arquivo AGENTS.md da raiz do repositório (se não tiver acesso a ele, as regras principais são:
nunca inventar fatos técnicos; código e comentários em inglês; cada crate segue o seu SPEC.md; nada de unsafe em código portável;
nunca entrar em pânico com entrada não confiável; testes rápidos e determinísticos; não mexer fora do escopo da tarefa;
não rodar nada como administrador nem gravar tela/áudio sem o usuário confirmar).

## Tarefa nº <número de TAREFAS.md> — <título>
- Objetivo: <o que precisa existir no fim>
- Contrato: <caminho do SPEC.md, ou cole o SPEC aqui>
- Escopo: pode alterar SOMENTE <pastas>. O resto do repositório é só leitura.
- Branch: crie `gpt/<nome-curto>` a partir de `claude/sync-gameplay-clip-app-xpgfwx`.
- Pronto quando:
  - <critérios de aceite>
  - todas as checagens passam:
    cargo fmt --all
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace
    (no worker: npm run typecheck && npm test)
  - docs/TAREFAS.md atualizado: status "em revisão", branch e resumo de uma linha.

## Entrega
Faça commits em português na branch da tarefa, e no fim me mostre: o que fez, quantos testes passam, desvios do SPEC
e dúvidas. Se algo do SPEC parecer errado, pergunte em vez de improvisar.
````

## Boas tarefas para delegar

Tarefas com **contrato claro, escopo isolado e testável sem o PC Windows** são as melhores para outra IA:
- Worker (TypeScript): rotas novas com testes no vitest (ex.: presença por grupo, tarefa 10).
- Crates portáveis com SPEC pronto (lógica pura, testes unitários).
- Revisão cruzada: pedir que o GPT revise o código que o Claude fez, e vice-versa.

Tarefas que dependem do **Windows real** (captura, áudio, encoder) funcionam melhor com quem roda no seu PC, porque precisam de execução e
medição na máquina.
