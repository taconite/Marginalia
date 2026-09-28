'use client';

import React, {
  useCallback,
  useEffect,
  useRef,
  useState,
  type MouseEvent as ReactMouseEvent,
  type PointerEvent as ReactPointerEvent,
} from 'react';
import { FiRefreshCw, FiSend, FiX } from 'react-icons/fi';
import { BsPin, BsPinFill } from 'react-icons/bs';
import clsx from 'clsx';
import DOMPurify from 'dompurify';
import { marked } from 'marked';

import { useChatStore } from '@/store/chatStore';
import { useEnv } from '@/context/EnvContext';
import { useBookDataStore } from '@/store/bookDataStore';
import { useReaderStore } from '@/store/readerStore';
import {
  buildPiPrompt,
  listPiSessions,
  resolveLocalPdfPath,
  preparePiDocument,
  sendPiPrompt,
  subscribePiMessages,
  type PiSession,
  unsubscribePiMessages,
} from '@/services/pi/bridge';

type ChatMessage = { id: number } & (
  | { role: 'user'; text: string; selectedText?: string }
  | { role: 'assistant'; html: string }
);

function errorMessage(cause: unknown, fallback: string): string {
  if (cause instanceof Error) return cause.message;
  return typeof cause === 'string' ? cause : fallback;
}

function openAssistantLink(event: ReactMouseEvent<HTMLDivElement>): void {
  const target = event.target;
  if (!(target instanceof Element)) return;
  const link = target.closest('a[href]');
  if (!(link instanceof HTMLAnchorElement)) return;

  event.preventDefault();
  if (link.protocol === 'http:' || link.protocol === 'https:') {
    globalThis.open(link.href, '_blank', 'noopener,noreferrer');
  }
}

function renderAssistantHtml(content: string): string {
  return DOMPurify.sanitize(marked.parse(content) as string, {
    USE_PROFILES: { html: true },
    FORBID_TAGS: [
      'audio',
      'embed',
      'iframe',
      'img',
      'object',
      'source',
      'style',
      'track',
      'video',
    ],
    FORBID_ATTR: [
      'action',
      'background',
      'data',
      'formaction',
      'poster',
      'src',
      'srcset',
      'style',
    ],
  });
}

const ChatPanel: React.FC = () => {
  const { envConfig } = useEnv();
  const {
    isOpen,
    isPinned,
    panelWidth,
    pendingContext: storedContext,
    bookKey: chatBookKey,
    reconcileBooks,
    selectedSocketPath,
    connection, setConnection,
    setOpen,
    setPinned,
    setPanelWidth,
    setPendingContext,
    setSelectedSocketPath,
  } = useChatStore();
  const bookKeys = useReaderStore((state) => state.bookKeys);
  const bookKey = chatBookKey && bookKeys.includes(chatBookKey) ? chatBookKey : null;
  const pendingContext = storedContext?.bookKey === bookKey ? storedContext : null;
  useEffect(() => reconcileBooks(bookKeys), [bookKeys, reconcileBooks]);
  const progress = useReaderStore((state) => state.getProgress(bookKey ?? ''));
  const currentChapter = pendingContext?.chapterTitle ?? progress?.sectionLabel ?? '';
  const currentPage = pendingContext?.page ?? progress?.page ?? null;

  const isResizing = useRef(false);
  const preparation = useRef<string | null>(null);
  const cancelPreparation = useCallback(() => {
    if (preparation.current) void unsubscribePiMessages(preparation.current).catch(() => undefined);
    preparation.current = null;
    setNotice(null);
  }, []);
  useEffect(() => cancelPreparation, [cancelPreparation]);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const nextMessageId = useRef(0);
  const [sessions, setSessions] = useState<PiSession[]>([]);
  const [input, setInput] = useState('');
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [subscriptionRevision, setSubscriptionRevision] = useState(0);
  const [isLoadingSessions, setIsLoadingSessions] = useState(false);
  const [subscribedSocketPath, setSubscribedSocketPath] = useState<string | null>(null);
  const activeSubscription = useRef<string | null>(null);
  const [isSending, setIsSending] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [waitingSince, setWaitingSince] = useState<number | null>(null);
  const [waitingSeconds, setWaitingSeconds] = useState(0);

  useEffect(() => {
    if (waitingSince === null) return;
    const timer = setInterval(() => {
      setWaitingSeconds(Math.floor((Date.now() - waitingSince) / 1000));
    }, 1000);
    return () => clearInterval(timer);
  }, [waitingSince]);

  const refreshSessions = useCallback(async () => {
    setIsLoadingSessions(true);
    setError(null);
    try {
      const discovered = await listPiSessions(connection);
      if (useChatStore.getState().connection === connection) setSessions(discovered);
    } catch (cause) {
      if (useChatStore.getState().connection !== connection) return;
      setError(errorMessage(cause, 'Unable to discover Pi sessions'));
    } finally {
      if (useChatStore.getState().connection === connection) setIsLoadingSessions(false);
    }
  }, [connection]);

  useEffect(() => {
    if (!isOpen) return;
    void refreshSessions();
    setTimeout(() => inputRef.current?.focus(), 100);
  }, [isOpen, refreshSessions]);
  useEffect(() => { setSessions([]); setMessages([]); setNotice(null); cancelPreparation(); }, [connection, cancelPreparation]);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ block: 'end' });
  }, [isOpen, messages]);

  const handleResizeStart = (event: ReactPointerEvent) => {
    event.preventDefault();
    isResizing.current = true;
    const startX = event.clientX;
    const startWidth = panelWidth;

    const onMove = (moveEvent: globalThis.PointerEvent) => {
      const newWidth = Math.max(280, Math.min(800, startWidth + startX - moveEvent.clientX));
      setPanelWidth(newWidth);
    };
    const onUp = () => {
      isResizing.current = false;
      document.removeEventListener('pointermove', onMove);
      document.removeEventListener('pointerup', onUp);
    };

    document.addEventListener('pointermove', onMove);
    document.addEventListener('pointerup', onUp);
  };

  const selectedSession = sessions.find((session) => session.socketPath === selectedSocketPath);
  const selectedSessionSocketPath = selectedSession?.socketPath;
  const isSubscribed = subscribedSocketPath === selectedSessionSocketPath;

  useEffect(() => {
    activeSubscription.current = null;
    setSubscribedSocketPath(null);
    setWaitingSince(null);
    if (!selectedSessionSocketPath) return;

    const subscriptionId = crypto.randomUUID();
    let active = true;
    const unsubscribe = () => {
      void unsubscribePiMessages(subscriptionId).catch(() => undefined);
    };
    const isCurrent = () =>
      active && useChatStore.getState().connection === connection && useChatStore.getState().selectedSocketPath === selectedSessionSocketPath;

    void subscribePiMessages(subscriptionId, selectedSessionSocketPath, (event) => {
      if (event.type === 'subscribed' && !isCurrent()) {
        unsubscribe();
        return;
      }
      if (!isCurrent()) return;
      if (event.type === 'subscribed') {
        activeSubscription.current = subscriptionId;
        setSubscribedSocketPath(selectedSessionSocketPath);
        setError(null);
        return;
      }
      if (!event.content.trim()) return;
      setWaitingSince(null);
      const html = renderAssistantHtml(event.content);
      setMessages((current) => [
        ...current,
        {
          id: ++nextMessageId.current,
          role: 'assistant',
          html,
        },
      ]);
    }, connection)
      .then(() => {
        if (!isCurrent()) return;
        activeSubscription.current = null;
        setSubscribedSocketPath(null);
        setError('Pi session disconnected');
        setWaitingSince(null);
      })
      .catch((cause) => {
        if (!isCurrent()) return;
        activeSubscription.current = null;
        setSubscribedSocketPath(null);
        setError(errorMessage(cause, 'Unable to mirror Pi messages'));
        setWaitingSince(null);
      });

    return () => {
      active = false;
      activeSubscription.current = null;
      unsubscribe();
    };
  }, [selectedSessionSocketPath, subscriptionRevision, connection]);

  const handleSend = async () => {
    if (!bookKey || !useReaderStore.getState().bookKeys.includes(bookKey) || (!input.trim() && !pendingContext) || !selectedSession || !isSubscribed || isSending) return;

    const question = input.trim() || 'What does this passage mean?';
    const subscriptionId = activeSubscription.current;
    const operationId = crypto.randomUUID();
    preparation.current = operationId;
    let sending = false;

    setIsSending(true);
    setError(null);
    setNotice(null);
    try {
      const book = bookKey && useBookDataStore.getState().getBookData(bookKey)?.book;
      if (!book) throw new Error('Unable to identify the document to send to Pi');
      const progress = useReaderStore.getState().getProgress(bookKey);
      const localPath = await resolveLocalPdfPath(await envConfig.getAppService(), book);
      if (preparation.current !== operationId || !useReaderStore.getState().bookKeys.includes(bookKey)) return;
      if (connection.mode === 'ssh' && localPath) setNotice('Checking PDF cache and transferring if needed…');
      const pdfPath = await preparePiDocument(localPath, connection, operationId);
      if (preparation.current !== operationId || useChatStore.getState().connection !== connection || !useReaderStore.getState().bookKeys.includes(bookKey)) return;
      if (useChatStore.getState().selectedSocketPath !== selectedSession.socketPath) return;
      if (!subscriptionId || activeSubscription.current !== subscriptionId) {
        throw new Error('Pi subscription disconnected or changed. Reconnect and send again.');
      }
      const message = buildPiPrompt(question, {
        pdfPath,
        bookTitle: pendingContext?.bookTitle || book.title,
        bookAuthor: pendingContext?.bookAuthor || book.author,
        chapterTitle: pendingContext?.chapterTitle ?? progress?.sectionLabel ?? '',
        page: pendingContext?.page ?? progress?.page ?? null,
        selectedText: pendingContext?.selectedText || '',
        surroundingText: pendingContext?.surroundingText || '',
      });
      setNotice(null);
      preparation.current = null;
      sending = true;
      setWaitingSeconds(0);
      setWaitingSince(Date.now());
      const userMessage: ChatMessage = {
        id: ++nextMessageId.current, role: 'user', text: question, selectedText: pendingContext?.selectedText,
      };
      await sendPiPrompt(selectedSession.socketPath, message, connection);
      if (useChatStore.getState().connection !== connection || useChatStore.getState().selectedSocketPath !== selectedSession.socketPath) return;
      setMessages((current) => [...current, userMessage].sort((a, b) => a.id - b.id));
      setInput('');
      if (useChatStore.getState().pendingContext === pendingContext) setPendingContext(null);
      if (inputRef.current) inputRef.current.style.height = 'auto';
    } catch (cause) {
      if (sending) setWaitingSince(null);
      setNotice(null);
      setError(errorMessage(cause, 'Unable to send prompt to Pi') + (sending ? ' Check Pi before retrying; delivery may have occurred.' : ''));
    } finally {
      preparation.current = null;
      setNotice(null);
      setIsSending(false);
    }
  };

  const handleKeyDown = (event: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      void handleSend();
    } else if (event.key === 'Escape') {
      setOpen(false);
    }
  };

  if (!isOpen) return null;

  return (
    <div
      className={clsx(
        'chat-panel flex flex-col border-l border-base-300 bg-base-100',
        isPinned ? 'relative' : 'fixed right-0 top-0 bottom-0 z-[45] shadow-xl',
      )}
      style={{ width: panelWidth, minWidth: 280, maxWidth: 800 }}
    >
      <div
        className='absolute left-0 top-0 bottom-0 w-1 cursor-col-resize hover:bg-primary/30 active:bg-primary/50 z-10'
        onPointerDown={handleResizeStart}
      />

      <div className='flex items-center gap-1 border-b border-base-300 px-3 py-2'>
        <span className='font-medium text-sm'>Pi</span>
        <div className='flex-1' />
        <button
          className='btn btn-ghost btn-xs btn-square'
          onClick={() => {
            if (!isSubscribed) setSubscriptionRevision((revision) => revision + 1);
            void refreshSessions();
          }}
          disabled={isLoadingSessions}
          title='Refresh Pi sessions'
        >
          <FiRefreshCw size={14} className={isLoadingSessions ? 'animate-spin' : ''} />
        </button>
        <button
          className='btn btn-ghost btn-xs btn-square'
          onClick={() => setPinned(!isPinned)}
          title={isPinned ? 'Unpin' : 'Pin'}
        >
          {isPinned ? <BsPinFill size={14} /> : <BsPin size={14} />}
        </button>
        <button
          className='btn btn-ghost btn-xs btn-square'
          onClick={() => setOpen(false)}
          title='Close'
        >
          <FiX size={14} />
        </button>
      </div>

      <div className='border-b border-base-300 px-3 py-3 space-y-2'>
        <label className='text-xs'>Connection
          <select className='select select-bordered select-sm w-full' value={connection.mode} disabled={isSending}
            onChange={(event) => setConnection({ ...connection, mode: event.target.value as 'local' | 'ssh' })}>
            <option value='local'>Local</option><option value='ssh'>SSH</option>
          </select>
        </label>
        {connection.mode === 'ssh' && <input key={connection.host} className='input input-bordered input-sm w-full'
          aria-label='SSH host alias' placeholder='SSH host alias' defaultValue={connection.host} disabled={isSending}
          onBlur={(event) => { if (event.target.value.trim() !== connection.host) setConnection({ ...connection, host: event.target.value.trim() }); }} />}
        {isLoadingSessions && <p className='text-xs'>Connecting…</p>}
        {isSending && preparation.current && <button className='btn btn-xs' onClick={cancelPreparation}>Cancel</button>}
        <label className='text-xs font-medium' htmlFor='pi-session-picker'>
          Pi session
        </label>
        <select
          id='pi-session-picker'
          disabled={isSending}
          className='select select-bordered select-sm w-full text-xs'
          value={selectedSocketPath}
          onChange={(event) => {
            setSelectedSocketPath(event.target.value);
            setMessages([]);
            setNotice(null);
            setError(null);
          }}
        >
          <option value=''>Select a live session</option>
          {selectedSocketPath && !selectedSession && (
            <option value={selectedSocketPath} disabled>
              Disconnected session
            </option>
          )}
          {sessions.map((session) => (
            <option key={session.socketPath} value={session.socketPath}>
              {session.cwd} (PID {session.pid})
            </option>
          ))}
        </select>
        {sessions.length === 0 && !isLoadingSessions && !error && (
          <p className='text-xs text-base-content/50'>
            No live session found. Start one with :PiSession in Neovim.
          </p>
        )}
        {selectedSession && (
          <p className='text-xs text-base-content/50 truncate' title={selectedSession.socketPath}>
            {isSubscribed ? 'Mirroring' : 'Connecting to'} Pi in {selectedSession.cwd}.
          </p>
        )}
      </div>

      <div className='flex-1 overflow-y-auto px-3 py-3 space-y-3'>
        {!bookKey && <p className='text-xs text-base-content/70'>Open Ask Pi from the document you want to discuss.</p>}
        {currentChapter && (
          <div className='text-xs text-base-content/50'>
            {currentChapter}
            {currentPage !== null ? ` · page ${currentPage}` : ''}
          </div>
        )}
        {messages.map((message) => message.role === 'user' ? (
          <div key={message.id} className='ml-6 rounded-lg bg-primary/10 px-3 py-2 text-sm text-base-content break-words'>
            <div className='mb-1 text-xs font-medium'>You</div>
            {message.selectedText && (
              <blockquote className='mb-2 border-l-2 border-primary/30 pl-2 text-xs italic whitespace-pre-wrap'>
                {message.selectedText}
              </blockquote>
            )}
            <div className='whitespace-pre-wrap'>{message.text}</div>
          </div>
        ) : (
          // Nested anchors retain native keyboard activation; this only redirects navigation.
          // eslint-disable-next-line jsx-a11y/click-events-have-key-events, jsx-a11y/no-static-element-interactions
          <div
            key={message.id}
            onClick={openAssistantLink}
            className='mr-6 rounded-lg bg-base-200 px-3 py-2 text-sm text-base-content'
          >
            <div
              className='prose prose-sm max-w-none break-words text-current'
              dangerouslySetInnerHTML={{ __html: message.html }}
            />
          </div>
        ))}
        {pendingContext ? (
          <div className='rounded-lg bg-base-200 px-3 py-2'>
            <div className='flex items-start gap-2'>
              <div className='flex-1 text-xs text-base-content/70 line-clamp-6 italic'>
                &ldquo;{pendingContext.selectedText}&rdquo;
              </div>
              <button
                className='btn btn-ghost btn-xs btn-square shrink-0'
                onClick={() => setPendingContext(null)}
                title='Remove selected text'
              >
                <FiX size={12} />
              </button>
            </div>
          </div>
        ) : messages.length === 0 ? (
          <div className='flex items-center justify-center h-full text-center text-base-content/30 text-sm'>
            Select text in the document or ask Pi a question about your current location.
          </div>
        ) : null}
        {notice && <div className='alert alert-success text-xs'>{notice}</div>}
        {error && <div className='alert alert-error text-xs'>{error}</div>}
        <div ref={messagesEndRef} />
      </div>

      <div className='border-t border-base-300 px-3 py-2'>
        {waitingSince !== null && (
          <div role='status' className='mb-2 flex items-center gap-2 text-xs text-base-content/70'>
            <span aria-hidden='true' className='loading loading-spinner loading-xs' />
            <span>Waiting for the next Pi message…</span>
            <span aria-live='off'>{waitingSeconds}s</span>
          </div>
        )}
        <div className='flex items-center gap-2 rounded-lg border border-base-300 bg-base-100 px-2 py-2 focus-within:border-primary'>
          <textarea
            ref={inputRef}
            className='flex-1 resize-none bg-transparent text-sm outline-none min-h-[20px] max-h-[120px] placeholder:text-base-content/40'
            placeholder={pendingContext ? 'Ask Pi about this selection...' : 'Ask Pi about this document...'}
            value={input}
            onChange={(event) => {
              setInput(event.target.value);
              event.target.style.height = 'auto';
              event.target.style.height = `${Math.min(event.target.scrollHeight, 120)}px`;
            }}
            onKeyDown={handleKeyDown}
            rows={1}
            disabled={isSending}
          />
          <button
            className='btn btn-xs btn-circle btn-primary'
            onClick={() => void handleSend()}
            disabled={
              !bookKey || (!input.trim() && !pendingContext) || !selectedSession || !isSubscribed || isSending
            }
            title={
              !selectedSession
                ? 'Select a Pi session first'
                : isSubscribed
                  ? 'Send to Pi'
                  : 'Connecting to Pi'
            }
          >
            {isSending ? <span className='loading loading-spinner loading-xs' /> : <FiSend size={12} />}
          </button>
        </div>
      </div>
    </div>
  );
};

export default ChatPanel;
