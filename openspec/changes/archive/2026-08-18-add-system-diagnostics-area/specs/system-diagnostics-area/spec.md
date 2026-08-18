## Purpose

Defines the dashboard's **System** area — the fifth data area that hosts the
runtime diagnostics views — and how the existing navigation, header controls
and localization behave once it exists.

## ADDED Requirements

### Requirement: System is a fifth data area

The dashboard SHALL offer **System** as an additional data area alongside
Energy data, Events, Device statistics and Grid quality. It SHALL appear as
the last entry of the desktop sidebar and of the mobile data-area grid, use
the same selected/unselected presentation as the existing entries, and mark
itself as the current area when selected.

Selecting an area SHALL remain the only way to switch areas; the existing four
areas SHALL keep their current position, labels and behavior.

#### Scenario: System is reachable on desktop

- **WHEN** the dashboard is shown at desktop width
- **THEN** the sidebar lists System after Grid quality, and activating it
  shows the System area and marks the entry as the current page

#### Scenario: System is reachable on small screens

- **WHEN** the dashboard is shown at a width that uses the mobile data-area
  grid
- **THEN** System is offered as an additional touch target in that grid and
  activating it shows the System area

### Requirement: System area tabs

The System area SHALL contain exactly two tabs, **Transmissions** and
**Application log**, presented in the same header position as the period tabs
of the Energy data area. Transmissions SHALL be selected when the area is
opened. Exactly one tab SHALL be visible at a time, and only the visible tab
SHALL refresh its data.

#### Scenario: Transmissions is the default tab

- **WHEN** the operator opens the System area
- **THEN** the Transmissions tab is selected and its content is shown

#### Scenario: Switching tabs switches content

- **WHEN** the operator selects the Application log tab
- **THEN** the application log content replaces the transmissions content, and
  the transmissions content stops refreshing

#### Scenario: Tab choice is kept while in the area

- **WHEN** the operator selects the Application log tab, switches to another
  data area, and returns to System
- **THEN** the Application log tab is selected again

### Requirement: Header controls in the System area

The period tabs of the Energy data area SHALL NOT be shown in the System area.
The inverter filter SHALL remain available in the header, and the card title
SHALL name the System area together with the selected inverter scope, matching
the existing areas.

The selected inverter SHALL restrict the Transmissions tab to entries that
address or are answered by that inverter, and SHALL leave the Application log
tab unfiltered, because log records are not attributed to a single inverter.

#### Scenario: Period tabs are hidden

- **WHEN** the System area is shown
- **THEN** no day/week/month/year tabs are present

#### Scenario: Inverter filter narrows transmissions

- **WHEN** one inverter is selected in the header and the Transmissions tab is
  shown
- **THEN** only transmissions addressing or answered by that inverter are
  listed

#### Scenario: Inverter filter does not narrow the log

- **WHEN** one inverter is selected in the header and the Application log tab
  is shown
- **THEN** all retained log records are listed, and the view states that the
  log is not filtered by inverter

### Requirement: System area is read-only and localized

The System area SHALL be read-only: it SHALL offer no control that changes
configuration, deletes stored diagnostics, or triggers a poll. All of its labels, column
headings, level names, outcome names, empty states and error messages SHALL be
available in both English and German and SHALL follow the language selected in
the header, like the rest of the dashboard. Times SHALL be rendered in the
browser's local time using the same formatting as the other areas.

#### Scenario: No mutating control is offered

- **WHEN** either System tab is shown
- **THEN** it offers only view controls — refresh pause, filters and level
  selection — and no control that writes to the service

#### Scenario: Labels follow the selected language

- **WHEN** the operator switches the header language to German
- **THEN** every label, column heading and empty state of both System tabs is
  shown in German

### Requirement: System area handles an unreachable API

Each System tab SHALL keep its last successfully loaded content when a refresh
fails, SHALL state that the data could not be refreshed, and SHALL resume
normal refreshing once a later request succeeds.

#### Scenario: Refresh failure is reported without losing content

- **WHEN** the service becomes unreachable while a System tab is refreshing
- **THEN** the tab keeps the entries it already loaded and states that the
  refresh failed

#### Scenario: Recovery resumes refreshing

- **WHEN** the service becomes reachable again
- **THEN** the tab clears the failure notice and resumes adding new entries
