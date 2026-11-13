# Segurança

O carteiro fica exposto na porta 25 da internet e guarda um token com acesso
de escrita à caixa do Gmail. Falhas de segurança (relay aberto, contorno da
verificação DMARC, execução de código, vazamento do token, negação de
serviço barata) **não** vão em issue pública: use
[Report a vulnerability](https://github.com/LLawli/carteiro/security/advisories/new)
na aba Security do repositório.

Só a versão mais recente recebe correção.

## O que já se espera dele

- Nunca aceitar destinatário fora de `dominio` e `enderecos`.
- Nunca enviar e-mail (nem bounce).
- Não registrar o corpo das mensagens nem segredos no log.
- Rodar sem root, com a unit endurecida de `deploy/carteiro.service`.
