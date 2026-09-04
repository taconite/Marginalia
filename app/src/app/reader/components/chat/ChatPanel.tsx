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
import {
  buildPiPrompt,
  listPiSessions,
  sendPiPrompt,
  subscribePiMessages,
  type PiSession,
  unsubscribePiMessages,
} from '@/services/pi/bridge';

interface ChatMessage {
  id: number;
  html: string;
}

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
  const {
    isOpen,
    isPinned,
    panelWidth,
    pendingContext,
    bookTitle,
    bookAuthor,
    currentChapter,
    currentPage,
    selectedSocketPath,
    setOpen,
    setPinned,
    setPanelWidth,
    setPendingContext,
    setSelectedSocketPath,
  } = useChatStore();

  const isResizing = useRef(false);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const nextMessageId = useRef(0);
  const [sessions, setSessions] = useState<PiSession[]>([]);
  const [input, setInput] = useState('');
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [subscriptionRevision, setSubscriptionRevision] = useState(0);
  const [isLoadingSessions, setIsLoadingSessions] = useState(false);
  const [subscribedSocketPath, setSubscribedSocketPath] = useState<string | null>(null);
  const [isSending, setIsSending] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refreshSessions = useCallback(async () => {
    setIsLoadingSessions(true);
    setError(null);
    try {
      setSessions(await listPiSessions());
    } catch (cause) {
      setSessions([]);
      setError(errorMessage(cause, 'Unable to discover Pi sessions'));
    } finally {
      setIsLoadingSessions(false);
    }
  }, []);

  useEffect(() => {
    if (!isOpen) return;
    void refreshSessions();
    setTimeout(() => inputRef.current?.focus(), 100);
  }, [isOpen, refreshSessions]);

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
    setSubscribedSocketPath(null);
    if (!selectedSessionSocketPath) return;

    const subscriptionId = crypto.randomUUID();
    let active = true;
    const unsubscribe = () => {
      void unsubscribePiMessages(subscriptionId).catch(() => undefined);
    };
    const isCurrent = () =>
      active && useChatStore.getState().selectedSocketPath === selectedSessionSocketPath;

    void subscribePiMessages(subscriptionId, selectedSessionSocketPath, (event) => {
      if (event.type === 'subscribed' && !isCurrent()) {
        unsubscribe();
        return;
      }
      if (!isCurrent()) return;
      if (event.type === 'subscribed') {
        setSubscribedSocketPath(selectedSessionSocketPath);
        setError(null);
        return;
      }
      if (!event.content.trim()) return;
      const html = renderAssistantHtml(event.content);
      setMessages((current) => [
        ...current,
        {
          id: ++nextMessageId.current,
          html,
        },
      ]);
    })
      .then(() => {
        if (!isCurrent()) return;
        setSubscribedSocketPath(null);
        setError('Pi session disconnected');
      })
      .catch((cause) => {
        if (!isCurrent()) return;
        setSubscribedSocketPath(null);
        setError(errorMessage(cause, 'Unable to mirror Pi messages'));
      });

    return () => {
      active = false;
      unsubscribe();
    };
  }, [selectedSessionSocketPath, subscriptionRevision]);

  const handleSend = async () => {
    if ((!input.trim() && !pendingContext) || !selectedSession || !isSubscribed || isSending) return;

    const question = input.trim() || 'What does this passage mean?';
    const message = buildPiPrompt(question, {
      bookTitle: pendingContext?.bookTitle || bookTitle,
      bookAuthor: pendingContext?.bookAuthor || bookAuthor,
      chapterTitle: pendingContext?.chapterTitle || currentChapter,
      page: pendingContext?.page ?? currentPage,
      selectedText: pendingContext?.selectedText || '',
      surroundingText: pendingContext?.surroundingText || '',
    });

    setIsSending(true);
    setError(null);
    setNotice(null);
    try {
      await sendPiPrompt(selectedSession.socketPath, message);
      if (useChatStore.getState().selectedSocketPath !== selectedSession.socketPath) return;
      setInput('');
      setPendingContext(null);
      if (inputRef.current) inputRef.current.style.height = 'auto';
      setNotice(`Queued in Pi for ${selectedSession.cwd}`);
    } catch (cause) {
      setError(errorMessage(cause, 'Unable to send prompt to Pi'));
    } finally {
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
            setSubscriptionRevision((revision) => revision + 1);
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
        <label className='text-xs font-medium' htmlFor='pi-session-picker'>
          Pi session
        </label>
        <select
          id='pi-session-picker'
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
        {currentChapter && (
          <div className='text-xs text-base-content/50'>
            {currentChapter}
            {currentPage !== null ? ` · page ${currentPage}` : ''}
          </div>
        )}
        {messages.map((message) => (
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
              (!input.trim() && !pendingContext) || !selectedSession || !isSubscribed || isSending
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
