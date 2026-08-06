import { describe, it, expect } from 'vitest';
import en from '../locales/en';

// t() substitutes /\{(\w+)\}/ — single braces. An i18next-style `{{count}}` therefore
// keeps its outer pair and renders as the literal "{3}" on screen.
//
// Component tests cannot catch this: they all mock `t`, so the locale file's
// placeholder syntax is never exercised against the real implementation.
const SUBSTITUTES = /\{(\w+)\}/g;

const strings = () => Object.entries(en).filter(([, v]) => typeof v === 'string');

describe('en locale placeholders', () => {
  it('uses the single-brace form t() substitutes', () => {
    const offenders = strings()
      .filter(([, v]) => v.includes('{{'))
      .map(([k]) => k);

    expect(offenders).toEqual([]);
  });

  it('leaves no half-open brace that would survive substitution', () => {
    // `{count` or `count}` renders as itself and reads as a typo rather than a value.
    const offenders = strings()
      .filter(([, v]) => {
        const balanced = v.replace(SUBSTITUTES, '');
        return balanced.includes('{') || balanced.includes('}');
      })
      .map(([k]) => k);

    expect(offenders).toEqual([]);
  });
});
