import { createIoScope } from './io.mjs';
import { run } from './runner.mjs';

export async function ioCancellation(origin) {
  const preCancelled = createIoScope(origin, 'pre-cancelled');
  const preSignal = new AbortController();
  preSignal.abort();
  const [early, unaffected] = await Promise.all([
    run('pre-cancelled', 'io-exchange', { scope: preCancelled, signal: preSignal.signal }),
    run('unaffected', 'io-exchange', { scope: createIoScope(origin, 'unaffected') }),
  ]);
  if (early.io !== 'aborted' || early.shutdown !== 'clean' || preCancelled.stats.started !== 0 || unaffected.body !== 'unaffected') {
    throw new Error('Pre-cancelled I/O affected another event');
  }
  const scope = createIoScope(origin, 'cancel', { slow: true });
  const controller = new AbortController();
  const result = run('cancel', 'io-exchange', { scope, signal: controller.signal });
  await scope.ready;
  controller.abort();
  const response = await result;
  await scope.settled();
  if (response.io !== 'aborted' || response.shutdown !== 'clean' || scope.stats.headers !== 1 || scope.stats.aborted !== 1 || scope.pendingCount !== 0) {
    throw new Error('I/O cancellation did not drain');
  }
  return { io_cancellation: 'passed', pre_cancelled: 'no-fetch', ...scope.stats, pending: scope.pendingCount, shutdown: response.shutdown };
}

export async function ioRecovery(origin) {
  const scope = createIoScope(origin, 'abandoned', { slow: true });
  const active = run('abandoned', 'io-exchange', { scope });
  // Attach rejection handling before triggering another operation's failure.
  const activeOutcome = Promise.allSettled([active]);
  await scope.ready;
  const trap = await Promise.allSettled([run('trap-with-io', 'async-trap')]);
  const outcomes = [...await activeOutcome, ...trap];
  await scope.settled();
  if (outcomes.some(outcome => outcome.status !== 'rejected' || outcome.reason.code !== 'instance_abandoned')) {
    throw new Error('In-flight I/O generation was not abandoned');
  }
  if (!scope.aborted || scope.stats.headers !== 1 || scope.stats.aborted !== 1 || scope.stats.completed !== 0 || scope.pendingCount !== 0) {
    throw new Error('Abandoned I/O was not aborted');
  }
  const after = await run('fresh', 'io-exchange', { scope: createIoScope(origin, 'fresh') });
  if (after.body !== 'fresh' || after.shutdown !== 'clean' || after.generation <= outcomes[0].reason.generation) {
    throw new Error('New generation I/O did not recover');
  }
  return { io_recovery: 'passed', ...scope.stats, pending: scope.pendingCount, fresh_generation: after.generation, abandoned_shutdown: 'unconfirmed' };
}
