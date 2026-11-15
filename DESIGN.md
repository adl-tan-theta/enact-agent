# Design

## Source of truth

Status: Active for the gateway pairing surface. Date: 2026-11-09.
The companion `enact-launch-kit` checkout owns Enact identity: `AGENTS.md`,
`brand/STYLE-GUIDE.md`, `brand/LOGO-USAGE.md`, `brand/tokens/brand-tokens.css`,
and `website/website-tokens.css`. Assets and pairing CSS here are distribution
snapshots from launch-kit commit `acb8ee7ed0ea36403ccfe9d217b8aac3d1749e74`.
`brand/enact-liquid-glass-token-spec.md` supplies the dark semantic palette;
its broader native material and preference proposal is not implemented here.
Evidence reviewed: those sources, `web/src/App.tsx`, `web/src/index.css`,
`web/src/contexts/ThemeContext.tsx`, and the reported pairing screenshot.

## Brand

Warm Continuity v2 uses the supplied continuity mark, Plum, Clay, Orchid,
warm paper and DM Sans. Use the color mark on light fields and the white
mark on dark fields. Keep marks proportional, still, and free of effects.

## Product goals

Make gateway identity and the next pairing action clear. Preserve device
authorization, recovery commands, and useful error feedback.

## Personas and jobs

An operator opens a local or remote gateway and authorizes a browser device.
The operator may need the exact CLI recovery command on the gateway host.

## Information architecture

Pairing presents identity, current pairing state, code generation or CLI
recovery, code entry, and the Pair action in that order.

## Design principles

Use one scoped token mapping for the pairing surface. Keep authorization
behavior in its existing owner. Honor saved theme mode without rewriting
appearance preferences.

## Visual language

Use launch-kit website typography, 12px controls, 20px inset panels and 32px
outer cards. Light uses Cream, Ink and Plum; dark uses the supplied dark
semantic roles. Text-bearing surfaces are opaque. The logo has no animation.

## Components

`PairingDialog` owns the flow. `web/public/enact-pairing.css` owns its scoped
visual tokens and recipes. Existing buttons, inputs and feedback reuse those
tokens. Supplied mark files remain the dashboard logo variants.

## Accessibility

Keep keyboard focus visible, controls at least 44px high, text labels and
errors explicit, and case-sensitive codes free of autocorrection. Preserve
browser zoom and reduced motion. Rendering and accessibility acceptance
require actual light/dark, narrow/wide and keyboard checks.

## Responsive behavior

Use a single card capped at 28rem with viewport padding and wrapping recovery
commands. Allow content to grow vertically without clipping at narrow widths.

## Interaction states

Retain loading, generating, already-paired, CLI recovery, invalid-code error,
disabled submit and successful pairing behavior. Visual changes add no new
authorization or network state.

## Content voice

Keep operational wording direct. Use the branded public command in recovery
examples. Preserve existing translation keys and protocol identifiers.

## Implementation constraints

React, TypeScript and the existing Vite/Tailwind build remain the host.
Pairing CSS is a static asset so its relative bundled font path works beneath
the gateway path prefix. DM Sans ships locally with its SIL OFL license.
`ThemeContext` remains the sole owner of saved appearance preferences.
Revert the pairing source/assets to roll back; no private config migration
or authorization changes are required.

## Open questions

- [ ] Owner: product design. Decide any broader dashboard theme migration;
  this pairing update does not approve the native liquid-glass proposal.
