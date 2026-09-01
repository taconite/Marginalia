import { create } from 'zustand';
import { persist } from 'zustand/middleware';

export interface ChatContext {
  selectedText: string;
  surroundingText: string;
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

  bookTitle: string;
  bookAuthor: string;
  currentChapter: string;
  currentPage: number | null;
  selectedSocketPath: string;

  togglePanel: () => void;
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

      bookTitle: '',
      bookAuthor: '',
      currentChapter: '',
      currentPage: null,
      selectedSocketPath: '',

      togglePanel: () => set((s) => ({ isOpen: !s.isOpen })),
      setOpen: (open) => set({ isOpen: open }),
      setPinned: (pinned) => set({ isPinned: pinned }),
      setPanelWidth: (width) => set({ panelWidth: width }),
      setPendingContext: (ctx) => set({ pendingContext: ctx }),
      updateBookContext: (title, author, chapter, page) =>
        set({ bookTitle: title, bookAuthor: author, currentChapter: chapter, currentPage: page }),
      setSelectedSocketPath: (path) => set({ selectedSocketPath: path }),
    }),
    {
      name: 'marginalia-chat',
      partialize: (state) => ({
        isOpen: state.isOpen,
        isPinned: state.isPinned,
        panelWidth: state.panelWidth,
        selectedSocketPath: state.selectedSocketPath,
      }),
    },
  ),
);
