import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { Book } from '@/types/book';
import { useChatStore } from '@/store/chatStore';

const invokeMock = vi.hoisted(() => vi.fn());
const channels = vi.hoisted(() => [] as Array<{ onmessage?: (message: unknown) => void }>);

vi.mock('@tauri-apps/api/core', () => ({
  invoke: invokeMock,
  Channel: class {
    onmessage?: (message: unknown) => void;

    constructor() {
      channels.push(this);
    }
  },
}));

import {
  buildPiPrompt,
  listPiSessions,
  resolveLocalPdfPath,
  preparePiDocument,
  sendPiPrompt,
  subscribePiMessages,
  unsubscribePiMessages,
} from './bridge';

describe('Pi bridge', () => {
  const remote = { mode: 'ssh' as const, host: 'ubuntu-work' };
  it.each([0, 1, 2])('restores retained preferences from storage version %i', async (version) => {
    const preferences = { isOpen: true, isPinned: true, panelWidth: 520 };
    const connection = { selectedSocketPath: '/remote/pi.sock', connection: remote };
    const state = version === 1
      ? { ...preferences, provider: 'anthropic', modelId: 'old-model', webSearchEnabled: true }
      : { ...preferences, ...connection };
    useChatStore.setState(useChatStore.getInitialState(), true);
    localStorage.setItem('marginalia-chat', JSON.stringify({ version, state }));
    await useChatStore.persist.rehydrate();
    const expected = { ...preferences, ...(version === 1 ? { selectedSocketPath: '', connection: { mode: 'local', host: '' } } : connection) };
    expect(useChatStore.getState()).toMatchObject(expected);
    expect(useChatStore.getState()).not.toHaveProperty('provider');
    expect(useChatStore.getState()).not.toHaveProperty('modelId');
    expect(useChatStore.getState()).not.toHaveProperty('webSearchEnabled');
    expect(JSON.parse(localStorage.getItem('marginalia-chat')!)).toEqual({ version: 2, state: expected });
    useChatStore.setState(useChatStore.getInitialState(), true);
    localStorage.removeItem('marginalia-chat');
  });
  it('routes SSH discovery, prompts and subscriptions to the selected host', async () => {
    invokeMock.mockResolvedValue([]);
    await listPiSessions(remote);
    await sendPiPrompt('/remote/pi.sock', 'Question', remote);
    const onEvent = vi.fn();
    await subscribePiMessages('ssh-sub', '/remote/pi.sock', onEvent, remote);
    expect(invokeMock.mock.calls.map(([command, args]) => [command, args.sshHost, args.request])).toEqual([
      ['pi_ssh', remote.host, { op: 'discover' }],
      ['pi_ssh', remote.host, { op: 'relay', socketPath: '/remote/pi.sock', request: { type: 'prompt', message: 'Question' } }],
      ['pi_ssh', remote.host, { op: 'relay', socketPath: '/remote/pi.sock', request: { type: 'subscribe' } }],
    ]);
    expect(invokeMock.mock.calls[2]?.[1].operationId).toBe('ssh-sub');
    channels[2]?.onmessage?.({ type: 'assistant', content: 'Reply', timestamp: 1 });
    expect(onEvent).toHaveBeenCalledWith({ type: 'assistant', content: 'Reply', timestamp: 1 });
  });
  it('prepares a remote PDF and propagates transfer failures without sending', async () => {
    invokeMock.mockResolvedValueOnce({ path: '/remote/cache/digest.pdf' });
    await expect(preparePiDocument('/Mac/Paper With Spaces.pdf', remote, 'upload')).resolves.toBe('/remote/cache/digest.pdf');
    expect(invokeMock).toHaveBeenCalledWith('pi_ssh', expect.objectContaining({ operationId: 'upload', request: { op: 'receive', path: '/Mac/Paper With Spaces.pdf' } }));
    invokeMock.mockRejectedValueOnce(new Error('upload interrupted'));
    await expect(preparePiDocument('/Mac/paper.pdf', remote, 'failed')).rejects.toThrow('upload interrupted');
    expect(invokeMock).toHaveBeenCalledTimes(2);
    await expect(preparePiDocument('/local/paper.pdf', { mode: 'local', host: '' }, 'local')).resolves.toBe('/local/paper.pdf');
    await expect(preparePiDocument(undefined, remote, 'no-pdf')).resolves.toBeUndefined();
    expect(invokeMock).toHaveBeenCalledTimes(2);
  });
  it('clears the session when its connection changes', () => {
    useChatStore.getState().setSelectedSocketPath('/local/pi.sock');
    useChatStore.getState().setConnection(remote);
    expect(useChatStore.getState().selectedSocketPath).toBe('');
    useChatStore.getState().setConnection({ mode: 'local', host: '' });
  });
  it.each([
    ['another book opened', 'a', ['b'], 'b', false],
    ['panel restored', null, ['b'], 'b', false],
    ['target still open', 'a', ['a', 'b'], 'a', true],
    ['ambiguous remaining books', 'a', ['b', 'c'], null, false],
    ['all books closed', 'a', [], null, false],
    ['same book in a new view', 'a-old', ['a-new'], 'a-new', false],
  ] as const)('reconciles document context: %s', (_name, previous, keys, expected, keepSelection) => {
    const selection = { bookKey: previous ?? '', selectedText: 'Old passage', chapterTitle: '', bookTitle: '', bookAuthor: '', page: 1 };
    useChatStore.setState({ isOpen: true, bookKey: previous, pendingContext: previous ? selection : null });
    useChatStore.getState().reconcileBooks([...keys]);
    expect(useChatStore.getState().bookKey).toBe(expected);
    expect(useChatStore.getState().pendingContext).toBe(keepSelection ? selection : null);
  });

  const book = { format: 'PDF', hash: 'book-id', title: 'A Paper With Spaces' } as Book;
  const pdfPath = '/library/book-id/A Paper With Spaces.pdf';
  const read = vi.fn().mockResolvedValue(new ArrayBuffer(1));
  const close = vi.fn().mockResolvedValue(undefined);
  const file = { slice: () => ({ arrayBuffer: read }), close } as unknown as File;
  const service = {
    exists: vi.fn().mockResolvedValue(true),
    readDirectory: vi.fn().mockResolvedValue([]),
    resolveFilePath: vi.fn().mockResolvedValue(pdfPath),
    openFile: vi.fn().mockResolvedValue(file),
  };

  it.each(['selected passage', ''])('includes the readable PDF path with selection %j', async (selectedText) => {
    const path = await resolveLocalPdfPath(service, book);
    const prompt = buildPiPrompt('Question', {
      pdfPath: path,
      page: 1, selectedText,
    });
    expect(prompt).toContain(`PDF path: ${JSON.stringify(pdfPath)}`);
    expect(service.exists).toHaveBeenCalledWith('book-id/A Paper With Spaces.pdf', 'Books');
    expect(service.openFile).toHaveBeenCalledWith(pdfPath, 'None');
    expect(read).toHaveBeenCalled();
    expect(close).toHaveBeenCalled();
  });

  it('uses a transient file only when the library copy is absent', async () => {
    service.exists.mockResolvedValueOnce(false);
    await expect(resolveLocalPdfPath(service, { ...book, filePath: '/tmp/Paper.pdf' })).resolves.toBe('/tmp/Paper.pdf');
    expect(service.openFile).toHaveBeenLastCalledWith('/tmp/Paper.pdf', 'None');
  });
  it('uses the reader filename fallback when a library PDF title has changed', async () => {
    service.exists.mockResolvedValueOnce(false);
    service.readDirectory.mockResolvedValueOnce([{ path: 'cover.png' }, { path: 'Original Title.pdf' }]);
    service.resolveFilePath.mockResolvedValueOnce('/library/book-id/Original Title.pdf');
    await expect(resolveLocalPdfPath(service, book)).resolves.toBe('/library/book-id/Original Title.pdf');
    expect(service.readDirectory).toHaveBeenCalledWith('book-id', 'Books');
    expect(service.resolveFilePath).toHaveBeenLastCalledWith('book-id/Original Title.pdf', 'Books');
    expect(service.openFile).toHaveBeenLastCalledWith('/library/book-id/Original Title.pdf', 'None');
  });

  it('rejects missing files and unreadable PDFs, closing an opened file', async () => {
    service.exists.mockResolvedValueOnce(false);
    await expect(resolveLocalPdfPath(service, book)).rejects.toThrow('Book file not found');
    service.resolveFilePath.mockRejectedValueOnce(new Error('resolution failed'));
    await expect(resolveLocalPdfPath(service, book)).rejects.toThrow('resolution failed');
    read.mockRejectedValueOnce(new Error('permission denied'));
    close.mockClear();
    await expect(resolveLocalPdfPath(service, book)).rejects.toThrow('permission denied');
    expect(close).toHaveBeenCalledOnce();
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it('keeps non-PDF questions unchanged', async () => {
    await expect(resolveLocalPdfPath(service, { ...book, format: 'EPUB' })).resolves.toBeUndefined();
  });

  beforeEach(() => {
    invokeMock.mockReset();
    channels.length = 0;
  });

  it.each([7, null])('quotes only the selected passage and its known page (%s)', (page) => {
    expect(buildPiPrompt('Why?', { page, selectedText: 'First line.\nSecond line.' }))
      .toBe(`Selected passage${page === null ? '' : ' (page 7)'}:\n> First line.\n> Second line.\n\nWhy?`);
  });

  it('preserves an unselected question verbatim, adding only an introductory PDF path', () => {
    const question = '  Explain this.\nKeep this formatting.  ';
    const context = { page: null, selectedText: '' };
    expect(buildPiPrompt(question, context)).toBe(question);
    expect(buildPiPrompt(question, { ...context, pdfPath }))
      .toBe(`PDF path: ${JSON.stringify(pdfPath)}\n\n${question}`);
  });

  it('uses a useful default question and bounds excerpt context', () => {
    const prompt = buildPiPrompt('', {
      page: null,
      selectedText: `start${'x'.repeat(20_000)}tail`,
    });

    expect(prompt).toContain('start');
    expect(prompt).not.toContain('tail');
    expect(prompt).toContain('[excerpt truncated by Marginalia]');
    expect(prompt).toContain('What does this passage mean?');
  });

  it('invokes the native session and prompt commands', async () => {
    invokeMock.mockResolvedValueOnce([]).mockResolvedValueOnce(undefined);

    await expect(listPiSessions()).resolves.toEqual([]);
    await expect(sendPiPrompt('/tmp/pi.sock', 'Question')).resolves.toBeUndefined();

    expect(invokeMock).toHaveBeenNthCalledWith(1, 'list_pi_sessions');
    expect(invokeMock).toHaveBeenNthCalledWith(2, 'send_pi_prompt', {
      socketPath: '/tmp/pi.sock',
      message: 'Question',
    });
  });

  it('subscribes to assistant messages through a native channel', async () => {
    invokeMock.mockResolvedValue(undefined);
    const onEvent = vi.fn();

    await expect(
      subscribePiMessages('subscription-1', '/tmp/pi.sock', onEvent),
    ).resolves.toBeUndefined();

    expect(invokeMock).toHaveBeenCalledWith('subscribe_pi_messages', {
      subscriptionId: 'subscription-1',
      socketPath: '/tmp/pi.sock',
      onEvent: channels[0],
    });

    const event = { type: 'assistant' as const, content: 'Answer', timestamp: 42 };
    channels[0]?.onmessage?.(event);
    expect(onEvent).toHaveBeenCalledWith(event);

    await expect(unsubscribePiMessages('subscription-1')).resolves.toBeUndefined();
    expect(invokeMock).toHaveBeenLastCalledWith('unsubscribe_pi_messages', {
      subscriptionId: 'subscription-1',
    });
  });
});
