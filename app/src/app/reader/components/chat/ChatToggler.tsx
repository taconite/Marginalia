'use client';

import React from 'react';
import { LuMessageSquareText } from 'react-icons/lu';

import { useChatStore } from '@/store/chatStore';
import { useTranslation } from '@/hooks/useTranslation';
import { useResponsiveSize } from '@/hooks/useResponsiveSize';
import Button from '@/components/Button';

const ChatToggler: React.FC<{ bookKey: string }> = ({ bookKey }) => {
  const _ = useTranslation();
  const { isOpen, bookKey: chatBookKey, togglePanel } = useChatStore();
  const iconSize16 = useResponsiveSize(16);

  return (
    <Button
      icon={
        <LuMessageSquareText
          size={iconSize16}
          className={isOpen && chatBookKey === bookKey ? 'text-primary' : 'text-base-content'}
        />
      }
      onClick={() => togglePanel(bookKey)}
      label={_('Ask Pi')}
    />
  );
};

export default ChatToggler;
