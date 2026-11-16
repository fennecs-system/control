# control-bot

A pure, GenServer-style supervisor for persistent bots: one mailbox per bot with
topic cursors, coalescing, overflow resync, budgets, timers and turn identity.
Applications define messages (`Info`) and topics; the crate never inspects them.
A message may be context only (`Info::wakes` false): it is admitted and
delivered with the next turn but cannot start one or release a wait until a
merge makes it waking, for example at an application-defined digest bound.

Future expansion, without changing the core:

- New event sources (watches, webhooks, SSH, game events) are new `Info`
  variants and topic kinds in the application.
- Links between bots: a bot's stop or exhaustion as a message to subscribers.
- Richer restart specs: `Restart` has permanent and temporary (one-shot) bots.
- Selective receive: priorities or filters per topic.
