# LanDNS

<!-- impeccable:product-schema 1 -->

## Platform

web

Tauri desktop shell targeting macOS; not a hosted website.

## Stack

Tauri, React, and Meta Astryx with its Neutral theme. Rust owns the local DNS and HTTP proxy services.

## Users

Mac users sharing development services with devices on their local network, using a graphical interface instead of terminal commands. The user is new to networking and needs to know what to fill in without learning DNS, proxy, interface, or TTL terminology first.

## Product Purpose

Resolve arbitrary subdomains to the Mac's current LAN address, track address changes, and map named services to local ports.

## Capabilities and Constraints

- Graphical configuration and start/stop controls; no per-domain hosts edits.
- The main task is adding a local project by its existing localhost URL and choosing a readable access name. Saving a project enables the required DNS and HTTP forwarding service together.
- Keep the overview focused on adding local projects and opening their saved URLs. Manage project targets on the project page; put DNS diagnostics, network status, and network tuning in Settings → Advanced settings, collapsed by default. Keep UI copy short.
- DNS clients must use this Mac as their DNS server or reach it through a configured router resolver.
- DHCP reservations or router DNS updates are required to keep the DNS endpoint reachable after its IP changes.
- First version uses IPv4 and HTTP, with WebSocket proxying; HTTPS certificate distribution is outside this version.
- System service installation uses the macOS administrator prompt to install an independent copy of the current executable, managed by launchd.
- Development/build and git commands must not be run automatically.

## Brand Commitments

Follow Astryx's official application templates with their Neutral theme and default component styles and sizes. Build navigation and page structure with AppShell, SideNav, and layout spacing tokens. Keep the Chinese UI concise and service state clear.

## Evidence on Hand

Greenfield repository. No product metrics, existing customers or live service data are available; the UI must reflect actual service state.
