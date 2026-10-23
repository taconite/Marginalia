import { create } from 'zustand';
import { persist } from 'zustand/middleware';
import type { PiConnection } from '@/services/pi/bridge';

export interface ChatContext {
  bookKey: string;
  selectedText: string;
  chapterTitle: string;
  bookTitle: string;
  bookAuthor: string;
  page: number | null;
}

interface ChatState {
  isOpen: boolean;
  isPinned: boolean;
  panelWidth: number;
  pendingContext: ChatContext | null;
  bookKey: string | null;

  bookTitle: string;
  bookAuthor: string;
  currentChapter: string;
  currentPage: number | null;
  selectedSocketPath: string;
  connection: PiConnection;
  setConnection: (connection: PiConnection) => void;

  togglePanel: (bookKey: string) => void;
  reconcileBooks: (bookKeys: string[]) => void;
  setOpen: (open: boolean) => void;
  setPinned: (pinned: boolean) => void;
  setPanelWidth: (width: number) => void;
  setPendingContext: (ctx: ChatContext | null) => void;
  updateBookContext: (title: string, author: string, chapter: string, page: number | null) => void;
  setSelectedSocketPath: (path: string) => void;
}

export const useChatStore = create<ChatState>()(
  persist(
    (set) => ({
      isOpen: false,
      isPinned: false,
      panelWidth: 380,
      pendingContext: null,
      bookKey: null,

      bookTitle: '',
      bookAuthor: '',
      currentChapter: '',
      currentPage: null,
      selectedSocketPath: '',
      connection: { mode: 'local', host: '' },
      setConnection: (connection) => set({ connection, selectedSocketPath: '' }),

      togglePanel: (bookKey) => set((s) => ({
        bookKey, isOpen: s.bookKey !== bookKey || !s.isOpen,
        pendingContext: s.pendingContext?.bookKey === bookKey ? s.pendingContext : null,
      })),
      reconcileBooks: (keys) => set((s) => ({
        bookKey: s.bookKey && keys.includes(s.bookKey) ? s.bookKey : keys.length === 1 ? keys[0]! : null,
        pendingContext: s.pendingContext && keys.includes(s.pendingContext.bookKey) ? s.pendingContext : null,
      })),
      setOpen: (open) => set({ isOpen: open }),
      setPinned: (pinned) => set({ isPinned: pinned }),
      setPanelWidth: (width) => set({ panelWidth: width }),
      setPendingContext: (ctx) => set((s) => ({ pendingContext: ctx, bookKey: ctx?.bookKey ?? s.bookKey })),
      updateBookContext: (title, author, chapter, page) =>
        set({ bookTitle: title, bookAuthor: author, currentChapter: chapter, currentPage: page }),
      setSelectedSocketPath: (path) => set({ selectedSocketPath: path }),
    }),
    {
      name: 'marginalia-chat',
      version: 2,
      migrate: (persistedState) => {
        const state = persistedState as Partial<ChatState>;
        return {
          isOpen: state.isOpen ?? false,
          isPinned: state.isPinned ?? false,
          panelWidth: state.panelWidth ?? 380,
          selectedSocketPath: state.selectedSocketPath ?? '',
          connection: state.connection ?? { mode: 'local', host: '' },
        };
      },
      partialize: (state) => ({
        isOpen: state.isOpen,
        isPinned: state.isPinned,
        panelWidth: state.panelWidth,
        selectedSocketPath: state.selectedSocketPath,
        connection: state.connection,
      }),
    },
  ),
);
