import { Channel, invoke } from '@tauri-apps/api/core';
import type { Book } from '@/types/book';
import type { AppService } from '@/types/system';
import type { ClosableFile } from '@/utils/file';
import { resolveBookFile } from '@/services/bookService';

export interface PiConnection { mode: 'local' | 'ssh'; host: string }
function sshRequest<T>(connection: PiConnection, request: object, operationId = crypto.randomUUID(), onEvent?: Channel<PiSubscriptionEvent>): Promise<T> {
  return invoke<T>('pi_ssh', { sshHost: connection.host, operationId, request, onEvent: onEvent ?? new Channel<PiSubscriptionEvent>() });
}

export async function preparePiDocument(path: string | undefined, connection: PiConnection, operationId: string): Promise<string | undefined> {
  if (!path || connection.mode === 'local') return path;
  const result = await sshRequest<{ path: string }>(connection, { op: 'receive', path }, operationId);
  return result.path;
}

export interface PiSession {
  socketPath: string;
  cwd: string;
  pid: number;
  createdAt: number;
}

export interface ReadingContext {
  pdfPath?: string;
  page: number | null;
  selectedText: string;
}

export type PiSubscriptionEvent =
  | { type: 'subscribed' }
  | { type: 'assistant'; content: string; timestamp: number };

const MAX_SELECTED_TEXT_CHARS = 12_000;

function clipText(text: string, maxChars: number): string {
  const normalized = text.trim();
  if (normalized.length <= maxChars) return normalized;
  return `${normalized.slice(0, maxChars)}\n[excerpt truncated by Marginalia]`;
}

export async function resolveLocalPdfPath(
  service: Pick<AppService, 'exists' | 'readDirectory' | 'resolveFilePath' | 'openFile'>,
  book: Book,
): Promise<string | undefined> {
  if (book.format !== 'PDF') return undefined;
  const location = await resolveBookFile({
    exists: service.exists.bind(service), readDir: service.readDirectory.bind(service),
  }, book);
  const path = location.base === 'Books'
    ? await service.resolveFilePath(location.path, location.base)
    : location.path;
  if (!path || !/^(\/|[a-z]:[\\/])/i.test(path)) {
    throw new Error('Unable to resolve a local PDF for this document');
  }
  const file = await service.openFile(path, 'None');
  try {
    await file.slice(0, 1).arrayBuffer();
  } finally {
    if ('close' in file) await (file as ClosableFile).close();
  }
  return path;
}

export function buildPiPrompt(question: string, context: ReadingContext): string {
  const parts: string[] = [];
  if (context.pdfPath) parts.push(`PDF path: ${JSON.stringify(context.pdfPath)}`);
  const selectedText = clipText(context.selectedText, MAX_SELECTED_TEXT_CHARS);
  if (selectedText) {
    const page = context.page !== null ? ` (page ${context.page})` : '';
    parts.push(`Selected passage${page}:\n${selectedText.split('\n').map((line) => `> ${line}`).join('\n')}`);
  }
  parts.push(question.trim() || !selectedText ? question : 'What does this passage mean?');
  return parts.join('\n\n');
}

export function listPiSessions(connection?: PiConnection): Promise<PiSession[]> {
  if (connection?.mode === 'ssh') return sshRequest(connection, { op: 'discover' });
  return invoke<PiSession[]>('list_pi_sessions');
}

export function sendPiPrompt(socketPath: string, message: string, connection?: PiConnection): Promise<void> {
  if (connection?.mode === 'ssh') return sshRequest(connection, { op: 'relay', socketPath, request: { type: 'prompt', message } });
  return invoke<void>('send_pi_prompt', { socketPath, message });
}

export function subscribePiMessages(
  subscriptionId: string,
  socketPath: string,
  onEvent: (event: PiSubscriptionEvent) => void,
  connection?: PiConnection,
): Promise<void> {
  const channel = new Channel<PiSubscriptionEvent>();
  channel.onmessage = onEvent;
  if (connection?.mode === 'ssh') return sshRequest(connection, { op: 'relay', socketPath, request: { type: 'subscribe' } }, subscriptionId, channel);
  return invoke<void>('subscribe_pi_messages', { subscriptionId, socketPath, onEvent: channel });
}

export function unsubscribePiMessages(subscriptionId: string): Promise<void> {
  return invoke<void>('unsubscribe_pi_messages', { subscriptionId });
}
