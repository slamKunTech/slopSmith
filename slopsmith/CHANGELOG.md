# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed
- Settings page restructured into separate "Slopsmith" (core) and "Plugins" sections, with each plugin's settings rendered as a collapsible panel (collapsed by default). "Plugin Updates" moved into the Plugins section.

## [0.2.4] - 2026-04-22

### Added
- Version badge in navbar (`/api/version` endpoint + `VERSION` file)
- `CHANGELOG.md` and semantic versioning
- Step Mode plugin
- `gp2midi` improvements and expanded test coverage
- Note Detection plugin factory-pattern refactor with multi-instance/splitscreen support
- Per-panel note detection in Split Screen plugin with M/L/R channel routing for multi-input interfaces

### Fixed
- `SLOPPAK_CACHE_DIR` moved to `CONFIG_DIR` for AppImage compatibility
- Improved error message when plugin requirements fail to install
