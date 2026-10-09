# Plano da Fase D — aplicativo completo (decisão 20)

Objetivo do usuário: **tudo pelo aplicativo** (sem terminal) — gravar, clipar com os amigos, juntar os POVs num editor, exportar em alta
qualidade e deixar o clipe editado disponível para o grupo baixar pelo máximo de tempo que caiba no plano grátis do R2.
Critério da decisão 19: versão utilizável primeiro; detalhes depois.

## Fluxo final

1. O usuário abre o **DuoClip** (app com janela e ícone na bandeja). O gravador roda dentro do app (sem terminal).
2. Primeira vez: tela de boas-vindas → nome → o app cria a identidade e se cadastra no servidor → criar grupo ou **colar um código de convite**.
3. Ao abrir um jogo, o app grava e anuncia presença (heartbeat de 30 s, `duoclip-presence`); a sessão do grupo se forma sozinha.
4. Alguém aperta o atalho → o app dele registra o clipe e um **pedido de clipe** no servidor. Os outros PCs da sessão recebem o pedido na
   resposta do próximo heartbeat (≤ 30 s). Como o buffer guarda o passado, cada PC ainda tem os segundos antes do aperto e os depois
   (ring de pelo menos `antes + 30 s de atraso + depois + margens`).
5. Cada PC salva o próprio POV localmente e envia uma cópia **cifrada** ao R2 (`clips/…`, expira em **3 dias**) — o "clipe cru".
6. Na aba **Clipes do grupo**, qualquer um vê o clipe com os POVs disponíveis, baixa e abre no **editor**: POVs alinhados pelo relógio
   global, cortes, layout (lado a lado, PiP, alternando), volume de cada áudio, exportar em alta qualidade.
7. O clipe editado é enviado (cifrado) para `edited/…` e fica **sem prazo**, listado para o grupo baixar. Quando o uso do R2 chegar perto de
   10 GB, o app avisa o grupo e apaga os editados mais antigos.

## Divisão do trabalho

| # | Tarefa | Dono | Revisor |
|---|---|---|---|
| 25 | **App desktop (Tauri 2) com o gravador dentro**: janela com a interface do GPT (`apps/desktop`), bandeja, gravador como motor em segundo plano, configurações reais (`config.toml`), biblioteca lendo `Vídeos\DuoClip`, tela de amigos ligada ao `duoclip-net` (cadastro, criar grupo, convidar, entrar) e presença contínua (resolve o NET-1: só o app fala com o servidor) | Claude | GPT |
| 26 | **Worker: pedidos de clipe e clipes editados**: rota para registrar um pedido de clipe da sessão e entregá-lo na resposta do heartbeat aos membros do mesmo grupo ativo; registro e URLs assinadas para editados em `edited/{crew}/{clip}/…` sem prazo; listagem de clipes (crus e editados) por grupo; contabilidade de bytes e política "perto de 10 GB → aviso + apagar os editados mais antigos" | GPT | Claude |
| 27 | **Troca de POVs no app**: ao receber/criar um pedido, fixar e coletar o POV (`duoclip-buffer`), salvar local, cifrar (`duoclip-crypto`) e subir para `clips/`; baixar os POVs dos outros; relógio global (`duoclip-clock`, SNTP) para alinhar | Claude | GPT |
| 28 | **Editor e clipes do grupo na interface**: editor no WebView2 (WebCodecs) com os POVs alinhados, cortes, layouts e mixer; exportar em alta qualidade; enviar o editado; aba de clipes do grupo com download | GPT | Claude |

Ordem: 25 e 26 em paralelo → 27 (usa 26) e 28 (usa 25/26) em paralelo. Contratos entre as partes ficam nos SPECs de cada tarefa;
mudança de contrato é avisada pela caixa antes de implementar.

## Limites que continuam valendo
Sem injeção, sem borda amarela, sem administrador (o atalho funciona por consulta da tecla), nada de gravar sem o usuário saber,
segredos fora do Git, R2 privado com URLs assinadas, criptografia ponta a ponta também nos editados.
