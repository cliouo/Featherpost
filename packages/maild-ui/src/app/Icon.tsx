import type { IconName } from '../host/types';

const paths: Record<IconName, string> = {
  search: 'M10 3a7 7 0 100 14 7 7 0 000-14M15 15l6 6',
  activity: 'M3 12h4l3-7 4 14 3-7h4',
  mail: 'M3 5h18v14H3zM3 5l9 8 9-8',
  reply: 'M9 5L3 11l6 6M3 11h10c5 0 7 3 7 8',
  forward: 'M15 5l6 6-6 6M21 11H11c-5 0-7 3-7 8',
  inbox: 'M3 13h5l2 3h4l2-3h5M5 5h14l2 8v6H3v-6z',
  folder: 'M3 6h6l2 2h10v11H3z',
  sent: 'M4 4l16 8-16 8 3-8z',
  drafts: 'M4 20h4l10-10-4-4L4 16z',
  archive: 'M3 4h18v4H3zM5 8v12h14V8M10 12h4',
  trash: 'M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13',
  spam: 'M12 3l9 16H3zM12 10v4M12 17v.5',
  star: 'M12 3l2.7 5.7 6.3.8-4.6 4.3 1.2 6.2L12 17l-5.6 3 1.2-6.2L3 9.5l6.3-.8z',
  paperclip: 'M8 12l7-7a3 3 0 014 4l-9 9a5 5 0 01-7-7l8-8',
  settings: 'M12 8a4 4 0 100 8 4 4 0 000-8zM4 12h2M18 12h2M12 4v2M12 18v2M6.3 6.3l1.4 1.4M16.3 16.3l1.4 1.4M6.3 17.7l1.4-1.4M16.3 7.7l1.4-1.4',
  refresh: 'M20 12a8 8 0 01-14 5.3M4 12a8 8 0 0114-5.3M18 3v4h-4M6 21v-4h4',
  more: 'M6 12h.01M12 12h.01M18 12h.01',
  clip: 'M8 4h8v3H8zM6 7h12v13H6zM9 12h6M9 16h4',
  back: 'M15 6l-6 6 6 6',
  puzzle: 'M10 4h4v3a2 2 0 104 0v3h3v4h-3a2 2 0 100 4v3h-4v-3a2 2 0 10-4 0v3H7v-3a2 2 0 100-4H4v-4h3a2 2 0 100-4z',
};

export function Icon({ name }: { name: IconName }) {
  return (
    <svg className="icon" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
      <path d={paths[name]} />
    </svg>
  );
}
