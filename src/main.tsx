import React from 'react'
import ReactDOM from 'react-dom/client'
import { InternationalizationProvider, LayerProvider } from '@astryxdesign/core'
import { Theme, type ThemeMode } from '@astryxdesign/core/theme'
import { neutralTheme } from '@astryxdesign/theme-neutral/built'
import App from './App'
import './index.css'

function Root() {
  const [themeMode, setThemeMode] = React.useState<ThemeMode>(() => {
    const theme = document.documentElement.getAttribute('data-theme')
    return theme === 'light' || theme === 'dark' ? theme : 'system'
  })

  React.useEffect(() => {
    document.querySelector<HTMLMetaElement>('meta[name="color-scheme"]')!.content = themeMode === 'system' ? 'light dark' : themeMode
  }, [themeMode])

  return <InternationalizationProvider locale="en" overrides={{ en: { '@astryx.field.required': '' } }}><Theme theme={neutralTheme} mode={themeMode}><LayerProvider toast={{ position: 'topEnd' }}><App themeMode={themeMode} onThemeChange={setThemeMode} /></LayerProvider></Theme></InternationalizationProvider>
}

ReactDOM.createRoot(document.getElementById('root')!).render(<React.StrictMode><Root /></React.StrictMode>)
