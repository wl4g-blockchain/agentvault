import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { ApprovalsPage } from '../../src/pages/ApprovalsPage'

describe('ApprovalsPage', () => {
  it('requires both scenario and workflow-run context for decisions', () => {
    render(<ApprovalsPage />)

    expect(screen.getByLabelText('Scenario ID')).toBeTruthy()
    expect(screen.getByLabelText('Workflow run ID')).toBeTruthy()
    expect(screen.getByRole('button', { name: 'Load pending' })).toBeTruthy()
  })
})
