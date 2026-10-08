# Como delegar uma tarefa ou uma revisão para outra IA (GPT ou outra)

> Regra do projeto: **quem implementa não revisa.** O GPT revisa o Claude e o Claude revisa o GPT ([`AGENTS.md`](../AGENTS.md), item 5).

## Fluxo local por arquivos

Quando ambos os agentes têm acesso ao PC, use [`COMUNICACAO-AGENTES.md`](COMUNICACAO-AGENTES.md).
O remetente publica o pedido diretamente na caixa compartilhada da pasta principal e o destinatário responde por outro arquivo.
O usuário não precisa copiar prompts, resultados de revisão ou entregas entre as conversas.
Os modelos abaixo continuam úteis para compor o conteúdo de cada mensagem e para agentes sem acesso ao mesmo disco.
As etapas de enviar ou pedir uma revisão pelo chat só se aplicam ao caso sem acesso compartilhado.

1. Escolha uma tarefa **livre** em [`TAREFAS.md`](TAREFAS.md) (ou peça ao Claude para criar uma, com SPEC).
2. Mude o "Dono" para `GPT` e o status para `em andamento` no quadro da pasta principal (ou peça ao Claude para fazer isso).
   No mesmo PC, registre os metadados localmente; push só é necessário para sincronizar outro ambiente.
3. Preencha o modelo abaixo e mande para o GPT:
   - **Com acesso à pasta do projeto** (ex.: um agente de terminal): ele lê o `AGENTS.md` sozinho, então basta a parte "Tarefa".
   - **Sem acesso** (chat): mande o modelo inteiro. Cole também o `SPEC.md` da tarefa e os arquivos que ele precisar ler.
4. Quando ele entregar, peça ao Claude: *"revise a entrega do GPT na branch `gpt/<tarefa>`"*. O Claude escreve `docs/revisoes/<tarefa>.md`,
   o GPT corrige o que for pedido, e com o veredito "aprovado" o Claude faz o merge.
5. Para o GPT **revisar** uma entrega do Claude, use o segundo modelo, mais abaixo ("Pedido de revisão").

## Modelo: pedido de implementação

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
- Branch: crie `gpt/<nome-curto>` a partir de `claude/sync-gameplay-clip-app-xpgfwx`, num worktree próprio em
  `C:\Users\bolad\Projetos\duoclip\worktrees\gpt-<nome-curto>`. A pasta principal permanece na integração.
- Pronto quando:
  - <critérios de aceite>
  - todas as checagens passam:
    cargo fmt --all
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace
    (no worker: npm run typecheck && npm test)
  - docs/TAREFAS.md da pasta principal atualizado: status "em revisão", branch e resumo de uma linha.
  - pedido publicado na caixa canônica, com o SHA exato, contrato e evidências.

## Entrega
Faça commits em português na branch da tarefa, e no fim me mostre: o que fez, quantos testes passam, desvios do SPEC
e dúvidas. Se algo do SPEC parecer errado, pergunte em vez de improvisar.
````

## Modelo: pedido de revisão (para o GPT revisar o Claude)

````text
Você vai REVISAR (não implementar) uma entrega do Claude no projeto DuoClip. Responda em português do Brasil.
Siga o AGENTS.md da raiz, item 5 (revisão cruzada). Você NÃO corrige o código: só aponta os problemas.

## Tarefa nº <número de TAREFAS.md> — <título>
- Branch a revisar: `claude/<tarefa>` · base: `claude/sync-gameplay-clip-app-xpgfwx`
  (veja o diff com: git diff claude/sync-gameplay-clip-app-xpgfwx...claude/<tarefa>)
- SHA exato a revisar: <commit informado na entrega>. Leia a branch pelo repositório principal e crie
  `gpt/revisao-<tarefa>` nesse SHA, em `worktrees/gpt-revisao-<tarefa>`, para executar as checagens.
  A pasta principal permanece na integração; o worktree do Claude não é alterado.
- Contrato: <caminho do SPEC.md>
- Rode as checagens (cargo fmt --all -- --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace).
  Se você não estiver no Windows, partes com #[cfg(windows)] e testes #[ignore] de hardware ficam como "não verificado".

## O que procurar
Bugs reais, não estilo: lógica errada, casos de borda, pânico com entrada não confiável, unsafe mal justificado, vazamento de
recursos, desvios do SPEC, testes que não testam o que dizem, e qualquer ponto em que as decisões do AGENTS.md não foram seguidas.
Para cada achado, mostre um cenário concreto que falha.

## Entrega
Crie docs/revisoes/<tarefa>.md na sua branch `gpt/revisao-<tarefa>` (commit em português, só o relatório),
no formato de docs/revisoes/README.md, com um veredito no fim: aprovado / aprovado com ressalvas / mudanças necessárias.
Se o código já está na integração, crie essa branch no SHA da integração que vai auditar.
Atualize o status em docs/TAREFAS.md direto na pasta de integração e publique o resultado na caixa canônica para-claude,
com o SHA revisado e o commit do relatório. O autor responde os achados pela caixa e corrige na própria branch;
você confere e atualiza o relatório sem editar a branch dele.
````

## Boas tarefas para delegar

Tarefas com **contrato claro, escopo isolado e testável sem o PC Windows** são as melhores para outra IA:
- Worker (TypeScript): rotas novas com testes no vitest (ex.: presença por grupo, tarefa 10).
- Crates portáveis com SPEC pronto (lógica pura, testes unitários).
- Revisão cruzada: pedir que o GPT revise o código que o Claude fez, e vice-versa.

Tarefas que dependem do **Windows real** (captura, áudio, encoder) funcionam melhor com quem roda no seu PC, porque precisam de execução e
medição na máquina.
