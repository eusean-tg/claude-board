import { describe, it, expect } from 'vitest';
import { COLUMNS, TASK_STATUSES, STATUS_DOT } from '../constants';
import en from '../../i18n/locales/en';
import { STATUS_COLORS } from '../../features/tasks/taskDetailHelpers';

// Every one of these lists is a separate hardcoded copy of the status set, and a
// status missing from any of them fails silently: the board drops the task from
// its grouping, the dot renders with an undefined class, the label shows a raw
// key. These tests exist so adding a status to the Rust state machine cannot
// half-land on the frontend.
describe('task status lists', () => {
  it('covers every status the backend state machine can emit', () => {
    // Mirrors TaskStatus in src-tauri/src/claude/state_machine.rs.
    expect([...TASK_STATUSES].sort()).toEqual(
      ['awaiting_approval', 'backlog', 'blocked', 'done', 'failed', 'in_progress', 'testing'].sort(),
    );
  });

  it('gives every status a column', () => {
    expect(COLUMNS.map((c) => c.id).sort()).toEqual([...TASK_STATUSES].sort());
  });

  it('gives every status a translated label', () => {
    for (const id of TASK_STATUSES) {
      expect(en[`status.${id}`], `missing i18n key status.${id}`).toBeTruthy();
    }
  });

  it('gives every status a dot colour', () => {
    for (const id of TASK_STATUSES) {
      expect(STATUS_DOT[id], `missing STATUS_DOT.${id}`).toBeTruthy();
    }
  });

  it('puts blocked directly after in_progress, which is also the list sort rank', () => {
    const ids = COLUMNS.map((c) => c.id);
    // A pause at the in_progress stage, not a stage further along. Rendering it
    // after Failed reads as a late state and is easy to miss.
    expect(ids[ids.indexOf('in_progress') + 1]).toBe('blocked');
  });

  it('gives every status a text colour', () => {
    for (const id of TASK_STATUSES) {
      expect(STATUS_COLORS[id], `missing STATUS_COLORS.${id}`).toBeTruthy();
    }
  });
});
